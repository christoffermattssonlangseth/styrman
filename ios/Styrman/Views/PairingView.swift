import SwiftUI
import VisionKit

/// First run: scan the QR code from `styr pair`, or type the address and code.
struct PairingView: View {
    @State private var scanned: PairingLink?
    @State private var showManual = false
    @State private var scannerError: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 24) {
                VStack(spacing: 12) {
                    WheelMark(size: 72)
                    Text("Pair with your Mac").font(.title.bold())
                    Text("On your Mac, run **styr pair** and point the camera at the QR code.")
                        .multilineTextAlignment(.center)
                        .foregroundStyle(.secondary)
                }
                .padding(.top, 24)

                if QRScanner.isAvailable {
                    QRScanner { link in scanned = link }
                        .clipShape(RoundedRectangle(cornerRadius: 20))
                        .overlay(RoundedRectangle(cornerRadius: 20).strokeBorder(.quaternary))
                        .frame(maxHeight: 360)
                } else {
                    ContentUnavailableView(
                        "Camera unavailable",
                        systemImage: "qrcode.viewfinder",
                        description: Text("Scan the code with the Camera app, or enter it by hand."))
                        .frame(maxHeight: 260)
                }

                Button("Enter address and code") { showManual = true }
                    .buttonStyle(.bordered)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 24)
            .navigationTitle("Styrman")
            .navigationBarTitleDisplayMode(.inline)
            .sheet(item: $scanned) { link in ConfirmPairingView(link: link) }
            .sheet(isPresented: $showManual) { ManualPairingView() }
        }
    }
}

struct ConfirmPairingView: View {
    @Environment(DaemonConnection.self) private var connection
    @Environment(\.dismiss) private var dismiss
    let link: PairingLink
    @State private var working = false
    @State private var error: String?

    var body: some View {
        NavigationStack {
            VStack(spacing: 20) {
                WheelMark(size: 56)
                Text("Pair with \(link.host)?").font(.title2.bold()).multilineTextAlignment(.center)
                Text(link.wsURL.host() ?? "").font(Theme.mono).foregroundStyle(.secondary)
                if let error {
                    Label(error, systemImage: "exclamationmark.triangle.fill")
                        .foregroundStyle(.red).font(.callout).multilineTextAlignment(.center)
                }
                Button {
                    Task { await pair() }
                } label: {
                    Group { if working { ProgressView() } else { Text("Pair") } }
                        .frame(maxWidth: .infinity).frame(height: 30)
                }
                .buttonStyle(.borderedProminent)
                .disabled(working)
                Spacer()
            }
            .padding(24)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
        }
        .presentationDetents([.medium])
    }

    private func pair() async {
        working = true
        defer { working = false }
        do {
            try await connection.pair(with: link, deviceName: UIDevice.current.name)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }
    }
}

struct ManualPairingView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var address = ""
    @State private var code = ""
    @State private var link: PairingLink?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("100.x.y.z:4719", text: $address)
                        .textInputAutocapitalization(.never).autocorrectionDisabled().keyboardType(.URL)
                        .font(Theme.mono)
                        .accessibilityLabel("Address")
                    TextField("Code", text: $code)
                        .textInputAutocapitalization(.characters).autocorrectionDisabled()
                        .font(Theme.mono)
                } footer: {
                    Text("Both are printed by `styr pair` under the QR code.")
                }
            }
            .navigationTitle("Enter code")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Next") { link = PairingLink(address: address, code: code) }
                        .disabled(PairingLink(address: address, code: code) == nil)
                }
            }
            .sheet(item: $link, onDismiss: { dismiss() }) { ConfirmPairingView(link: $0) }
        }
    }
}

/// VisionKit QR scanner that reports the first valid pairing link.
struct QRScanner: UIViewControllerRepresentable {
    let onLink: (PairingLink) -> Void

    @MainActor static var isAvailable: Bool {
        DataScannerViewController.isSupported && DataScannerViewController.isAvailable
    }

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let vc = DataScannerViewController(
            recognizedDataTypes: [.barcode(symbologies: [.qr])],
            qualityLevel: .balanced,
            isHighlightingEnabled: true)
        vc.delegate = context.coordinator
        try? vc.startScanning()
        return vc
    }

    func updateUIViewController(_ vc: DataScannerViewController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(onLink: onLink) }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        let onLink: (PairingLink) -> Void
        init(onLink: @escaping (PairingLink) -> Void) { self.onLink = onLink }

        func dataScanner(_ scanner: DataScannerViewController, didAdd items: [RecognizedItem], allItems: [RecognizedItem]) {
            for item in items {
                if case .barcode(let b) = item, let s = b.payloadStringValue, let url = URL(string: s),
                   let link = PairingLink(url: url) {
                    scanner.stopScanning()
                    onLink(link)
                    return
                }
            }
        }
    }
}
