// swift-tools-version:5.5
import PackageDescription

let package = Package(
    name: "tauri-plugin-mailcheck",
    platforms: [.iOS(.v15)],
    products: [
        .library(name: "tauri-plugin-mailcheck", type: .static, targets: ["tauri-plugin-mailcheck"])
    ],
    dependencies: [
        .package(name: "Tauri", path: "../.tauri/tauri-api")
    ],
    targets: [
        .target(name: "tauri-plugin-mailcheck", dependencies: [.byName(name: "Tauri")], path: "Sources")
    ]
)
