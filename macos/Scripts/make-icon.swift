#!/usr/bin/env swift
//
// Generates Resources/PairDrop.icns.
//
// The artwork is drawn here rather than checked in as a binary so it stays reviewable
// and easy to tweak. Deliberately not an SF Symbol: Apple's SF Symbols licence does not
// permit using them in app icons.
//
//   swift Scripts/make-icon.swift
//
import AppKit
import Foundation

let canvas = 1024

/// Design in a top-left origin space, like every drawing tool, and flip once at the end.
func point(_ x: CGFloat, _ y: CGFloat, in size: CGFloat) -> CGPoint {
    CGPoint(x: x / 100 * size, y: (100 - y) / 100 * size)
}

func drawIcon(size: CGFloat, in context: CGContext) {
    context.setShouldAntialias(true)
    context.interpolationQuality = .high

    // macOS icons sit in a rounded square inset from the edge of the canvas.
    let inset = size * 0.09
    let rect = CGRect(x: inset, y: inset, width: size - inset * 2, height: size - inset * 2)
    let corner = rect.width * 0.2237  // the macOS "squircle" ratio
    let squircle = CGPath(roundedRect: rect, cornerWidth: corner, cornerHeight: corner, transform: nil)

    // Body: a vertical gradient, deeper at the bottom.
    context.saveGState()
    context.addPath(squircle)
    context.clip()

    let colorSpace = CGColorSpaceCreateDeviceRGB()
    let gradient = CGGradient(colorsSpace: colorSpace, colors: [
        CGColor(srgbRed: 0.36, green: 0.44, blue: 0.98, alpha: 1),
        CGColor(srgbRed: 0.42, green: 0.28, blue: 0.90, alpha: 1),
        CGColor(srgbRed: 0.30, green: 0.20, blue: 0.72, alpha: 1)
    ] as CFArray, locations: [0, 0.55, 1])!
    context.drawLinearGradient(gradient,
                               start: CGPoint(x: rect.midX, y: rect.maxY),
                               end: CGPoint(x: rect.midX, y: rect.minY),
                               options: [])

    // A soft highlight across the top edge gives it some depth at large sizes.
    let sheen = CGGradient(colorsSpace: colorSpace, colors: [
        CGColor(srgbRed: 1, green: 1, blue: 1, alpha: 0.22),
        CGColor(srgbRed: 1, green: 1, blue: 1, alpha: 0)
    ] as CFArray, locations: [0, 1])!
    context.drawLinearGradient(sheen,
                               start: CGPoint(x: rect.midX, y: rect.maxY),
                               end: CGPoint(x: rect.midX, y: rect.midY),
                               options: [])
    context.restoreGState()

    // Paper plane: two facets sharing the fold, so it reads as folded paper rather
    // than a flat triangle. Kept well inside the squircle — the corners curve away
    // fast, and a glyph that reaches them looks like it is falling off the tile.
    let upper = CGMutablePath()
    upper.move(to: point(20, 50, in: size))
    upper.addLine(to: point(78, 24, in: size))
    upper.addLine(to: point(45, 57, in: size))
    upper.closeSubpath()

    let lower = CGMutablePath()
    lower.move(to: point(45, 57, in: size))
    lower.addLine(to: point(78, 24, in: size))
    lower.addLine(to: point(53, 78, in: size))
    lower.closeSubpath()

    context.saveGState()
    // Belt and braces: nothing escapes the tile.
    context.addPath(squircle)
    context.clip()
    context.setShadow(offset: CGSize(width: 0, height: -size * 0.012),
                      blur: size * 0.03,
                      color: CGColor(srgbRed: 0, green: 0, blue: 0, alpha: 0.28))
    context.addPath(upper)
    context.setFillColor(CGColor(srgbRed: 1, green: 1, blue: 1, alpha: 1))
    context.fillPath()

    // The underside facet is dimmer, as if turned away from the light.
    context.addPath(lower)
    context.setFillColor(CGColor(srgbRed: 0.82, green: 0.86, blue: 1.0, alpha: 1))
    context.fillPath()
    context.restoreGState()
}

func renderPNG(size: Int) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil,
                              pixelsWide: size, pixelsHigh: size,
                              bitsPerSample: 8, samplesPerPixel: 4,
                              hasAlpha: true, isPlanar: false,
                              colorSpaceName: .deviceRGB,
                              bytesPerRow: 0, bitsPerPixel: 0)!

    NSGraphicsContext.saveGraphicsState()
    let graphics = NSGraphicsContext(bitmapImageRep: rep)!
    NSGraphicsContext.current = graphics
    drawIcon(size: CGFloat(size), in: graphics.cgContext)
    NSGraphicsContext.restoreGraphicsState()

    return rep.representation(using: .png, properties: [:])!
}

// MARK: - Write the iconset

let scriptDirectory = URL(fileURLWithPath: CommandLine.arguments[0])
    .deletingLastPathComponent()
let root = scriptDirectory.pathComponents.contains("Scripts")
    ? scriptDirectory.deletingLastPathComponent()
    : URL(fileURLWithPath: FileManager.default.currentDirectoryPath)

let resources = root.appendingPathComponent("Resources")
let iconset = resources.appendingPathComponent("PairDrop.iconset")

try? FileManager.default.removeItem(at: iconset)
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)

// The names iconutil expects.
let variants: [(name: String, pixels: Int)] = [
    ("icon_16x16", 16), ("icon_16x16@2x", 32),
    ("icon_32x32", 32), ("icon_32x32@2x", 64),
    ("icon_128x128", 128), ("icon_128x128@2x", 256),
    ("icon_256x256", 256), ("icon_256x256@2x", 512),
    ("icon_512x512", 512), ("icon_512x512@2x", 1024)
]

for variant in variants {
    let data = renderPNG(size: variant.pixels)
    try data.write(to: iconset.appendingPathComponent("\(variant.name).png"))
}

// A standalone preview, handy for eyeballing changes.
try renderPNG(size: canvas).write(to: resources.appendingPathComponent("icon-preview.png"))

let process = Process()
process.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
process.arguments = ["-c", "icns",
                     iconset.path,
                     "-o", resources.appendingPathComponent("PairDrop.icns").path]
try process.run()
process.waitUntilExit()

guard process.terminationStatus == 0 else {
    FileHandle.standardError.write(Data("iconutil failed\n".utf8))
    exit(1)
}

try? FileManager.default.removeItem(at: iconset)
print("wrote \(resources.appendingPathComponent("PairDrop.icns").path)")
