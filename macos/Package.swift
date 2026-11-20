// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "HQC",
    platforms: [.macOS(.v14)],
    targets: [
        .executableTarget(
            name: "HQC",
            path: "Sources/HQC"
        ),
    ]
)
