import SwiftUI

/// Decorative only: the adjacent name and status are the accessible identity.
struct AgentAvatarView: View {
    let store: AgentAvatarStore
    let agentID: String
    let presentation: AgentIdentityPresentation
    var size: CGFloat = 48

    var body: some View {
        Group {
            if let image = store.image(agentID: agentID, expression: presentation.expression) {
                Image(uiImage: image).resizable().interpolation(.high).scaledToFit()
            } else {
                // Old daemons and failed images get a neutral system symbol, not a new face recipe.
                Image(systemName: "person.crop.square")
                    .resizable().scaledToFit().padding(size * 0.18).foregroundStyle(.secondary)
            }
        }
        .frame(width: size, height: size)
        .background(Color(uiColor: .secondarySystemBackground), in: RoundedRectangle(cornerRadius: size * 0.26))
        .clipShape(RoundedRectangle(cornerRadius: size * 0.26))
        .accessibilityHidden(true)
        .task(id: "\(store.sessionID)|\(store.retryID)|\(store.isOnline)|\(agentID)|\(presentation.expression.rawValue)") {
            store.request(agentID: agentID, expression: presentation.expression)
        }
    }
}
