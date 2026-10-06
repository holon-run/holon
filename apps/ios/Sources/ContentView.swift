import SwiftUI

struct ContentView: View {
    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                Image(systemName: "bubble.left.and.bubble.right")
                    .font(.largeTitle)
                    .accessibilityHidden(true)
                Text("welcome.title")
                    .font(.title)
                Text("welcome.message")
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .padding()
            .navigationTitle("Holon")
        }
    }
}
