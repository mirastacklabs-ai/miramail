// swift-tools-version:5.5
import PackageDescription

let package = Package(
    name: "tauri-plugin-bgrefresh",
    platforms: [.iOS(.v15)],
    products: [
        .library(name: "tauri-plugin-bgrefresh", type: .static, targets: ["tauri-plugin-bgrefresh"])
    ],
    dependencies: [
        .package(name: "Tauri", path: "../.tauri/tauri-api")
    ],
    targets: [
        .target(name: "tauri-plugin-bgrefresh", dependencies: [.byName(name: "Tauri")], path: "Sources")
    ]
)
