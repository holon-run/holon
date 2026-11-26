import SwiftUI

/// The same primary icon, visible status, and VoiceOver label on both identity surfaces.
struct AgentStatusLabel: View {
    let presentation: AgentIdentityPresentation
    var reservedLines: Int? = nil
    var showsTitle: Bool = true
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    static func tint(for presentation: AgentIdentityPresentation) -> Color {
        switch presentation.tone {
        case .muted: .secondary
        case .attention: .orange
        case .accent: .accentColor
        case .danger: .red
        }
    }

    private var color: Color { Self.tint(for: presentation) }

    @ViewBuilder
    private var iconView: some View {
        if presentation.animates(reduceMotion: reduceMotion) {
            ProgressView().controlSize(.mini)
                .frame(width: 14, height: 14)
        } else {
            Image(systemName: presentation.icon.systemName)
        }
    }

    var body: some View {
        Group {
            if showsTitle {
                HStack(spacing: 4) {
                    iconView
                    if let reservedLines {
                        Text(LocalizedStringKey(presentation.statusKey)).lineLimit(reservedLines, reservesSpace: true)
                    } else {
                        Text(LocalizedStringKey(presentation.statusKey))
                    }
                }
            } else {
                iconView
                    .frame(width: 16, height: 16)
            }
        }
        .foregroundStyle(color)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(LocalizedStringKey(presentation.statusKey)))
    }
}
