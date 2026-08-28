import Foundation
import Sparkle

private final class InstallHandler: @unchecked Sendable {
    private let handler: () -> Void

    init(_ handler: @escaping () -> Void) {
        self.handler = handler
    }

    func invoke() {
        handler()
    }
}

@MainActor
final class HolonUpdater: NSObject, SPUUpdaterDelegate {
    private lazy var controller = SPUStandardUpdaterController(
        startingUpdater: true,
        updaterDelegate: self,
        userDriverDelegate: nil
    )

    func checkForUpdates() {
        controller.checkForUpdates(nil)
    }

    nonisolated func updater(
        _ updater: SPUUpdater,
        shouldPostponeRelaunchForUpdate item: SUAppcastItem,
        untilInvokingBlock installHandler: @escaping () -> Void
    ) -> Bool {
        let continuation = InstallHandler(installHandler)
        DispatchQueue.global(qos: .userInitiated).async {
            if let executable = try? HolonBinaryLocator.resolve() {
                let process = Process()
                process.executableURL = executable
                process.arguments = ["daemon", "prepare-update"]
                process.standardOutput = FileHandle.nullDevice
                process.standardError = FileHandle.nullDevice
                try? process.run()
                process.waitUntilExit()
            }
            continuation.invoke()
        }
        return true
    }
}
