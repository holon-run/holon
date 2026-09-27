import CoreImage.CIFilterBuiltins
import SwiftUI

struct HolonQRCodeView: View {
    let payload: String

    var body: some View {
        Group {
            if let image = Self.image(for: payload) {
                Image(decorative: image, scale: 1, orientation: .up)
                    .interpolation(.none)
                    .resizable()
                    .scaledToFit()
                    .frame(width: 180, height: 180)
            } else {
                Text("Unable to generate QR code.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .accessibilityLabel("Connection QR code")
    }

    private static func image(for payload: String) -> CGImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(payload.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else {
            return nil
        }

        let context = CIContext()
        return context.createCGImage(output, from: output.extent)
    }
}
