import SwiftUI

struct ConnectionSettings: View {
    @AppStorage("ui.language") private var language = "system"

    var body: some View {
        Section("settings.title") {
            Picker("settings.language", selection: $language) {
                Text("settings.system").tag("system")
                Text("settings.english").tag("en")
                Text("settings.chinese").tag("zh-Hans")
            }
            Text("settings.languageHelp").font(.caption).foregroundStyle(.secondary)
        }
    }
}
