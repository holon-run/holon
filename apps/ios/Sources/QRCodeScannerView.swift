import SwiftUI
@preconcurrency import AVFoundation
import UIKit

@MainActor
struct QRCodeScannerView: View {
    let onScan: (String) -> Void
    @Environment(\.scenePhase) private var scenePhase
    @StateObject private var camera = QRScannerModel()
    @State private var visible = false

    init(onScan: @escaping (String) -> Void) {
        self.onScan = onScan
    }

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            QRScannerPreview(session: camera.capture.session)
                .ignoresSafeArea()
                .accessibilityHidden(true)
            VStack {
                Spacer()
                Text(LocalizedStringKey(camera.message))
                    .foregroundStyle(.white)
                    .padding()
                    .background(.black.opacity(0.75), in: RoundedRectangle(cornerRadius: 12))
                if camera.message == "scan.permissionDenied" {
                    Button("scan.openSettings") {
                        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
                        UIApplication.shared.open(url)
                    }
                    .buttonStyle(.borderedProminent)
                }
            }
            .padding()
        }
        .onAppear {
            visible = true
            camera.onScan = onScan
            camera.setActive(scenePhase == .active)
        }
        .onDisappear {
            visible = false
            camera.setActive(false)
        }
        .onChange(of: scenePhase) { _, phase in
            camera.setActive(visible && phase == .active)
        }
    }
}

@MainActor
private final class QRScannerModel: ObservableObject {
    @Published var message = "scan.requesting"
    let capture = QRScannerCapture()
    var onScan: ((String) -> Void)?
    private var active = false
    private var finished = false
    private var generation = 0

    func setActive(_ value: Bool) {
        active = value && !finished
        generation += 1
        let token = generation
        capture.setActive(active, generation: token) { [weak self] event in
            guard let self, self.active, !self.finished, self.generation == token else { return }
            switch event {
            case .message(let key):
                self.message = key
            case .scanned(let text):
                self.finished = true
                self.active = false
                self.capture.setActive(false, generation: token + 1, notify: { _ in })
                self.onScan?(text)
            }
        }
    }
}

// All mutable capture state belongs to queue; the gate only serializes lifecycle
// invalidation against startRunning. AVFoundation's session also backs the UI layer.
private final class QRScannerCapture: NSObject, AVCaptureMetadataOutputObjectsDelegate, @unchecked Sendable {
    enum Event: Sendable {
        case message(String)
        case scanned(String)
    }

    let session = AVCaptureSession()
    private let queue = DispatchQueue(label: "run.holon.ios.qr-camera")
    private let gate = NSLock()
    private var desiredActive = false
    private var generation = 0
    private var configured = false
    private var delivered = false
    private var requesting = false
    private var notify: (@MainActor @Sendable (Event) -> Void)?

    func setActive(
        _ active: Bool,
        generation: Int,
        notify: @escaping @MainActor @Sendable (Event) -> Void
    ) {
        gate.lock()
        desiredActive = active
        self.generation = generation
        gate.unlock()
        queue.async { [self] in
            self.notify = notify
            if isCurrent(generation) {
                delivered = false
                prepare(generation)
            } else if session.isRunning {
                session.stopRunning()
            }
        }
    }

    private func isCurrent(_ token: Int) -> Bool {
        gate.lock()
        defer { gate.unlock() }
        return desiredActive && generation == token
    }

    private func emit(_ event: Event) {
        guard let notify else { return }
        Task { @MainActor in notify(event) }
    }

    private func prepare(_ token: Int) {
        guard isCurrent(token) else { return }
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .notDetermined:
            emit(.message("scan.requesting"))
            guard !requesting else { return }
            requesting = true
            AVCaptureDevice.requestAccess(for: .video) { [weak self] _ in
                guard let self else { return }
                self.queue.async {
                    self.requesting = false
                    // Use the latest activation, not the activation that requested permission.
                    self.gate.lock()
                    let current = self.generation
                    self.gate.unlock()
                    self.prepare(current)
                }
            }
        case .denied:
            emit(.message("scan.permissionDenied"))
        case .restricted:
            emit(.message("scan.restricted"))
        case .authorized:
            configureAndStart(token)
        @unknown default:
            emit(.message("scan.failed"))
        }
    }

    private func configureAndStart(_ token: Int) {
        if !configured {
            guard let device = AVCaptureDevice.default(for: .video) else {
                emit(.message("scan.unavailable"))
                return
            }
            do {
                let input = try AVCaptureDeviceInput(device: device)
                let output = AVCaptureMetadataOutput()
                session.beginConfiguration()
                guard session.canAddInput(input) else {
                    session.commitConfiguration()
                    emit(.message("scan.failed"))
                    return
                }
                session.addInput(input)
                guard session.canAddOutput(output) else {
                    session.removeInput(input)
                    session.commitConfiguration()
                    emit(.message("scan.failed"))
                    return
                }
                session.addOutput(output)
                guard output.availableMetadataObjectTypes.contains(.qr) else {
                    session.removeOutput(output)
                    session.removeInput(input)
                    session.commitConfiguration()
                    emit(.message("scan.unavailable"))
                    return
                }
                output.setMetadataObjectsDelegate(self, queue: queue)
                output.metadataObjectTypes = [.qr]
                session.commitConfiguration()
                configured = true
            } catch {
                emit(.message("scan.failed"))
                return
            }
        }
        // Linearize start with synchronous page invalidation. If start is already
        // in flight, invalidation waits for it, then queues stop on this same queue.
        gate.lock()
        let shouldStart = desiredActive && generation == token && !delivered
        if shouldStart && !session.isRunning {
            session.startRunning()
        }
        gate.unlock()
        if shouldStart {
            emit(.message(session.isRunning ? "scan.aim" : "scan.failed"))
        }
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        gate.lock()
        let active = desiredActive && !delivered
        gate.unlock()
        guard active,
              let text = metadataObjects.compactMap({ object -> String? in
                  guard let qr = object as? AVMetadataMachineReadableCodeObject, qr.type == .qr else { return nil }
                  return qr.stringValue
              }).first else { return }
        delivered = true
        session.stopRunning()
        emit(.scanned(text))
    }
}

@MainActor
private struct QRScannerPreview: UIViewRepresentable {
    let session: AVCaptureSession

    func makeUIView(context: Context) -> PreviewView {
        let view = PreviewView()
        view.previewLayer.videoGravity = .resizeAspectFill
        view.previewLayer.session = session
        return view
    }

    func updateUIView(_ uiView: PreviewView, context: Context) {}

    static func dismantleUIView(_ uiView: PreviewView, coordinator: ()) {
        uiView.previewLayer.session = nil
    }

    final class PreviewView: UIView {
        override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }
        var previewLayer: AVCaptureVideoPreviewLayer { layer as! AVCaptureVideoPreviewLayer }
    }
}
