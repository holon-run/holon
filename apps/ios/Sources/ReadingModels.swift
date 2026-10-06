import Foundation
import HolonClient

struct ReadingAgent: Identifiable, Equatable, Codable, Sendable {
    let id: String
    let name: String
    let preview: String
    let operatorPreview: String?
    let unreadCount: Int?

    init(id: String, name: String, preview: String,
         operatorPreview: String? = nil, unreadCount: Int? = nil) {
        self.id = id
        self.name = name
        self.preview = preview
        self.operatorPreview = operatorPreview
        self.unreadCount = unreadCount
    }
}

enum ReadingStatus: String, Equatable {
    case disconnected, syncing, live, offline, permissionDenied, sessionExpired, incompatible
}

enum ReadingReadStatus: String, Equatable {
    case idle, pending, failed, confirmed
}

extension JSONValue {
    var readingString: String? {
        if case .string(let value) = self { return value }
        return nil
    }
    var readingInteger: Int64? {
        if case .integer(let value) = self { return value }
        return nil
    }
}

struct ReadingPartition: Hashable, Codable, Sendable {
    let api: String
    let network: String
    let runtime: String
    let user: String
    let visibility: String

    init?(apiBaseURL: URL, identity: HolonConnectionIdentity) {
        guard let runtime = identity.runtimeID, !runtime.isEmpty,
              let user = identity.userID, !user.isEmpty,
              let visibility = identity.visibilityScopeID, !visibility.isEmpty,
              var url = URLComponents(url: apiBaseURL, resolvingAgainstBaseURL: false),
              url.user == nil, url.password == nil else { return nil }
        url.query = nil
        url.fragment = nil
        guard let api = url.string else { return nil }
        self.api = api
        network = identity.networkID
        self.runtime = runtime
        self.user = user
        self.visibility = visibility
    }
}
