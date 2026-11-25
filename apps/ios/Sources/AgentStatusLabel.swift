import SwiftUI

/// The same primary icon, visible status, and VoiceOver label on both identity surfaces.
struct AgentStatusLabel: View {
    let presentation: AgentIdentityPresentation
    var reservedLines: Int? = nil
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var color: Color {
        switch presentation.tone {
        case .muted: .secondary
        case .attention: .orange
        case .accent: .accentColor
        case .danger: .red
        }
    }

    var body: some View {
        Label {
            if let reservedLines {
                Text(LocalizedStringKey(presentation.statusKey)).lineLimit(reservedLines, reservesSpace: true)
            } else {
                Text(LocalizedStringKey(presentation.statusKey))
            }
        } icon: {
            if presentation.animates(reduceMotion: reduceMotion) {
                ProgressView().controlSize(.mini)
                    .frame(width: 14, height: 14)
            } else {
                Image(systemName: presentation.icon.systemName)
            }
        }
        .foregroundStyle(color)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(LocalizedStringKey(presentation.statusKey)))
    }
}
