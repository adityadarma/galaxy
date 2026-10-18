#!/usr/bin/env swift
// Generates the Galaxy app icon (AppIcon.appiconset) with CoreGraphics.
// Design: warm paper squircle, ink-blue planet, terracotta orbit + moon.
// Flat, muted, no glow/sparkles. Re-run after tweaking constants below.
//
// Usage: swift scripts/generate-icon.swift [output.appiconset]

import AppKit
import CoreGraphics
import Foundation

// MARK: Palette

func rgb(_ hex: UInt32, _ a: CGFloat = 1) -> CGColor {
    CGColor(
        srgbRed: CGFloat((hex >> 16) & 0xFF) / 255,
        green: CGFloat((hex >> 8) & 0xFF) / 255,
        blue: CGFloat(hex & 0xFF) / 255,
        alpha: a
    )
}

let paperTop = rgb(0xF3EDE3)
let paperBottom = rgb(0xE7DED0)
let ink = rgb(0x2E3850)
let inkShade = rgb(0x262F45)
let clay = rgb(0xD27D56)
let edge = rgb(0x000000, 0.06)

// MARK: Drawing (1024 canvas, macOS grid: 824 body inset 100)

func drawIcon(_ ctx: CGContext) {
    let canvas: CGFloat = 1024
    let body = CGRect(x: 100, y: 100, width: 824, height: 824)
    let squircle = CGPath(roundedRect: body, cornerWidth: 186, cornerHeight: 186, transform: nil)

    // Soft drop shadow under the body.
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -10), blur: 24, color: rgb(0x000000, 0.22))
    ctx.addPath(squircle)
    ctx.setFillColor(paperBottom)
    ctx.fillPath()
    ctx.restoreGState()

    // Paper background, barely-there vertical tone shift.
    ctx.saveGState()
    ctx.addPath(squircle)
    ctx.clip()
    let paper = CGGradient(
        colorsSpace: CGColorSpace(name: CGColorSpace.sRGB),
        colors: [paperTop, paperBottom] as CFArray,
        locations: [0, 1]
    )!
    ctx.drawLinearGradient(
        paper,
        start: CGPoint(x: 0, y: body.maxY),
        end: CGPoint(x: 0, y: body.minY),
        options: []
    )

    let center = CGPoint(x: canvas / 2, y: canvas / 2 - 6)
    let planetR: CGFloat = 196
    let planet = CGRect(x: center.x - planetR, y: center.y - planetR, width: planetR * 2, height: planetR * 2)

    let tilt: CGFloat = -0.32  // radians, ring rises to the right... visually left-low
    let orbitW: CGFloat = 640
    let orbitH: CGFloat = 188
    let ringWidth: CGFloat = 22
    let gap: CGFloat = 14

    func orbitPath() -> CGPath {
        var t = CGAffineTransform(translationX: center.x, y: center.y).rotated(by: tilt)
        return CGPath(
            ellipseIn: CGRect(x: -orbitW / 2, y: -orbitH / 2, width: orbitW, height: orbitH),
            transform: &t
        )
    }

    // Clip to the front half of the orbit (below its major axis, toward viewer).
    func clipFront() {
        ctx.translateBy(x: center.x, y: center.y)
        ctx.rotate(by: tilt)
        ctx.clip(to: CGRect(x: -2000, y: -2000, width: 4000, height: 2000))
        ctx.rotate(by: -tilt)
        ctx.translateBy(x: -center.x, y: -center.y)
    }

    // 1. Back half of the ring.
    ctx.addPath(orbitPath())
    ctx.setStrokeColor(clay)
    ctx.setLineWidth(ringWidth)
    ctx.strokePath()

    // 2. Planet, with a flat terminator shade on the lower-right.
    ctx.addEllipse(in: planet)
    ctx.setFillColor(ink)
    ctx.fillPath()

    ctx.saveGState()
    ctx.addEllipse(in: planet)
    ctx.clip()
    let offset: CGFloat = 70
    ctx.addEllipse(in: planet.offsetBy(dx: -offset, dy: offset).insetBy(dx: -20, dy: -20))
    ctx.addRect(planet.insetBy(dx: -50, dy: -50))
    ctx.setFillColor(inkShade)
    ctx.fillPath(using: .evenOdd)
    ctx.restoreGState()

    // 3. Front half: paper-colored gap over the planet, then the ring.
    ctx.saveGState()
    clipFront()
    ctx.saveGState()
    ctx.addEllipse(in: planet)
    ctx.clip()
    ctx.addPath(orbitPath())
    ctx.setStrokeColor(paperTop)
    ctx.setLineWidth(ringWidth + gap * 2)
    ctx.strokePath()
    ctx.restoreGState()
    ctx.addPath(orbitPath())
    ctx.setStrokeColor(clay)
    ctx.setLineWidth(ringWidth)
    ctx.strokePath()
    ctx.restoreGState()

    // 4. Moon riding the ring, upper-right.
    let angle: CGFloat = 0.62  // parametric position on the ellipse
    let local = CGPoint(x: cos(angle) * orbitW / 2, y: sin(angle) * orbitH / 2)
    let moon = local.applying(CGAffineTransform(rotationAngle: tilt)).applying(
        CGAffineTransform(translationX: center.x, y: center.y))
    let moonR: CGFloat = 40
    ctx.addEllipse(in: CGRect(x: moon.x - moonR - 12, y: moon.y - moonR - 12, width: (moonR + 12) * 2, height: (moonR + 12) * 2))
    ctx.setFillColor(paperTop)
    ctx.fillPath()
    ctx.addEllipse(in: CGRect(x: moon.x - moonR, y: moon.y - moonR, width: moonR * 2, height: moonR * 2))
    ctx.setFillColor(clay)
    ctx.fillPath()

    ctx.restoreGState()

    // Hairline edge so the light icon holds on light backgrounds.
    ctx.addPath(CGPath(roundedRect: body.insetBy(dx: 0.75, dy: 0.75), cornerWidth: 185, cornerHeight: 185, transform: nil))
    ctx.setStrokeColor(edge)
    ctx.setLineWidth(1.5)
    ctx.strokePath()
}

// MARK: Render + export

func render(size: Int) -> Data {
    let cs = CGColorSpace(name: CGColorSpace.sRGB)!
    let ctx = CGContext(
        data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: 0,
        space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
    )!
    ctx.interpolationQuality = .high
    ctx.setShouldAntialias(true)
    let s = CGFloat(size) / 1024
    ctx.scaleBy(x: s, y: s)
    drawIcon(ctx)
    let rep = NSBitmapImageRep(cgImage: ctx.makeImage()!)
    return rep.representation(using: .png, properties: [:])!
}

let out = CommandLine.arguments.count > 1
    ? CommandLine.arguments[1]
    : "app/Galaxy/Resources/Assets.xcassets/AppIcon.appiconset"
let fm = FileManager.default
try fm.createDirectory(atPath: out, withIntermediateDirectories: true)

var images: [[String: String]] = []
for pt in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let px = pt * scale
        let name = "icon_\(pt)x\(pt)\(scale == 2 ? "@2x" : "").png"
        try render(size: px).write(to: URL(fileURLWithPath: "\(out)/\(name)"))
        images.append(["idiom": "mac", "size": "\(pt)x\(pt)", "scale": "\(scale)x", "filename": name])
    }
}

let contents: [String: Any] = ["images": images, "info": ["author": "xcode", "version": 1]]
let json = try JSONSerialization.data(withJSONObject: contents, options: [.prettyPrinted, .sortedKeys])
try json.write(to: URL(fileURLWithPath: "\(out)/Contents.json"))

let catalog = (out as NSString).deletingLastPathComponent
let catalogContents = "\(catalog)/Contents.json"
if !fm.fileExists(atPath: catalogContents) {
    try #"{"info":{"author":"xcode","version":1}}"#.write(toFile: catalogContents, atomically: true, encoding: .utf8)
}
print("Wrote \(out)")
