import Foundation
import Sparkle

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

    nonisolated func updater(_ updater: SPUUpdater, willInstallUpdate item: SUAppcastItem) {
        guard let executable = try? HolonBinaryLocator.resolve() else {
            return
        }
        let process = Process()
        process.executableURL = executable
        process.arguments = ["daemon", "prepare-update"]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try? process.run()
        process.waitUntilExit()
    }
}
