import SwiftUI

struct ConnectionSettings: View {
    @AppStorage("ui.language") private var language = "system"
    var showsHeading = true

    var body: some View {
        Section {
            Picker("settings.language", selection: $language) {
                Text("settings.system").tag("system")
                Text("settings.english").tag("en")
                Text("settings.chinese").tag("zh-Hans")
            }
            .font(.body)
            Text("settings.languageHelp").font(.body).foregroundStyle(.secondary)
        } header: {
            if showsHeading { Text("settings.title").font(.headline) }
        }
    }
}
