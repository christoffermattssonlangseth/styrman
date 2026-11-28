import SwiftUI
import UIKit

/// The app's Home Screen icon: following Dark Mode, or always one background.
enum AppIconChoice: String, CaseIterable, Identifiable {
    case automatic, white, black, grey

    var id: String { rawValue }

    /// The alternate icon's name (nil: the main one, which follows Dark Mode).
    var iconName: String? {
        switch self {
        case .automatic: nil
        case .white: "AppIcon-White"
        case .black: "AppIcon-Black"
        case .grey: "AppIcon-Grey"
        }
    }

    var label: String {
        switch self {
        case .automatic: "Automatic"
        case .white: "White"
        case .black: "Black"
        case .grey: "Dark grey"
        }
    }

    static var current: AppIconChoice {
        allCases.first { $0.iconName == UIApplication.shared.alternateIconName } ?? .automatic
    }
}

struct IconSettings: View {
    @State private var chosen = AppIconChoice.current
    @State private var failed: String?

    var body: some View {
        Section {
            HStack(spacing: 0) {
                ForEach(AppIconChoice.allCases) { choice in
                    Button { set(choice) } label: {
                        VStack(spacing: 6) {
                            IconPreview(choice: choice)
                                .overlay(
                                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                                        .strokeBorder(chosen == choice ? Theme.accent : Color.primary.opacity(0.12),
                                                      lineWidth: chosen == choice ? 2.5 : 1)
                                )
                            Text(choice.label)
                                .font(.caption)
                                .foregroundStyle(chosen == choice ? Theme.textPrimary : Theme.textSecondary)
                        }
                        .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("\(choice.label) icon")
                    .accessibilityAddTraits(chosen == choice ? .isSelected : [])
                }
            }
            .padding(.vertical, 6)
        } header: {
            Text("App icon")
        } footer: {
            Text(failed ?? "Automatic is white in Light Mode and black in Dark Mode.")
        }
    }

    private func set(_ choice: AppIconChoice) {
        guard choice != chosen else { return }
        let before = chosen
        chosen = choice
        failed = nil
        UIApplication.shared.setAlternateIconName(choice.iconName) { error in
            guard let error else { return }
            Task { @MainActor in
                chosen = before
                failed = "Couldn't change the icon: \(error.localizedDescription)"
            }
        }
    }
}

/// A small picture of an icon choice: its background and the wheel.
private struct IconPreview: View {
    let choice: AppIconChoice

    var body: some View {
        ZStack {
            switch choice {
            case .automatic:
                // Half light, half dark.
                HStack(spacing: 0) { Color.white; Color.black }
                WheelMark(size: 40).mask(HStack(spacing: 0) { Color.black; Color.clear })
                    .environment(\.colorScheme, .light)
                WheelMark(size: 40).mask(HStack(spacing: 0) { Color.clear; Color.black })
                    .environment(\.colorScheme, .dark)
            case .white:
                Color.white
                WheelMark(size: 40).environment(\.colorScheme, .light)
            case .black:
                Color.black
                WheelMark(size: 40).environment(\.colorScheme, .dark)
            case .grey:
                Color(UIColor(hex: 0x292928))
                WheelMark(size: 40).environment(\.colorScheme, .dark)
            }
        }
        .frame(width: 56, height: 56)
        .clipShape(RoundedRectangle(cornerRadius: 14, style: .continuous))
    }
}
