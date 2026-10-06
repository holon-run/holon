import Foundation

// Minimal model-only support for the pinned Swift5 generator. No generated HTTP client.
protocol JSONEncodable: Encodable {}

protocol CaseIterableDefaultsLast: Decodable, CaseIterable, RawRepresentable
where RawValue: Decodable, AllCases: BidirectionalCollection {}

extension CaseIterableDefaultsLast {
    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        let raw = try container.decode(RawValue.self)
        if let known = Self(rawValue: raw) {
            self = known
        } else if let fallback = Self.allCases.last {
            self = fallback
        } else {
            throw DecodingError.dataCorruptedError(in: container, debugDescription: "Empty wire enum")
        }
    }
}

protocol UnknownCaseCheckable {
    var containsUnknownDefaultOpenApiCase: Bool { get }
}

extension UnknownCaseCheckable {
    public var containsUnknownDefaultOpenApiCase: Bool { false }
}
