import SwiftUI

enum BriefPlaceholderState: Equatable {
    case unloaded, loading, failed, uncachedOffline

    init(isLoading: Bool, hasFailed: Bool, canLoad: Bool = true) {
        self = !canLoad ? .uncachedOffline : isLoading ? .loading : hasFailed ? .failed : .unloaded
    }

    var titleKey: String {
        switch self {
        case .unloaded: "reading.brief.unloaded"
        case .loading: "reading.brief.loading"
        case .failed: "reading.brief.failed"
        case .uncachedOffline: "reading.brief.uncachedOffline"
        }
    }

    var actionKey: String? {
        switch self {
        case .unloaded: "reading.brief.load"
        case .loading: nil
        case .failed: "reading.retry"
        case .uncachedOffline: nil
        }
    }
}

/// Missing content is conversation chrome, never an invented assistant message.
struct BriefPlaceholderView: View {
    let briefID: String
    let state: BriefPlaceholderState
    var load: () -> Void
    @Environment(\.dynamicTypeSize) private var textSize

    var body: some View {
        Group {
            if textSize.isAccessibilitySize {
                VStack(alignment: .leading, spacing: 4) {
                    status
                    action.frame(maxWidth: .infinity, alignment: .trailing)
                }
            } else {
                HStack(spacing: 8) {
                    status
                    Spacer(minLength: 8)
                    action
                }
            }
        }
        .font(.caption)
        .frame(minHeight: 44)
        .padding(.horizontal, 12)
        .padding(.vertical, 4)
        .background(Color(uiColor: .tertiarySystemFill), in: RoundedRectangle(cornerRadius: 10))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("brief.placeholder." + briefID)
    }

    private var status: some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            if state == .loading {
                ProgressView().controlSize(.mini).accessibilityHidden(true)
            } else {
                Image(systemName: state == .failed ? "exclamationmark.circle" : "doc.text")
                    .accessibilityHidden(true)
            }
            Text(LocalizedStringKey(state.titleKey))
                .fixedSize(horizontal: false, vertical: true)
        }
        .foregroundStyle(.secondary)
    }

    @ViewBuilder private var action: some View {
        if let key = state.actionKey {
            Button(action: load) {
                Label(LocalizedStringKey(key), systemImage: state == .failed ? "arrow.clockwise" : "arrow.down.circle")
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(minWidth: 44, minHeight: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(Color.accentColor)
            .accessibilityIdentifier("brief.load." + briefID)
        }
    }
}
