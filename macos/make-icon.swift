// Draws the app icon and writes an .iconset: `swift make-icon.swift <dir>`.
// build.sh turns it into AppIcon.icns with iconutil.
import AppKit

let out = URL(fileURLWithPath: CommandLine.arguments[1])
try FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

func render(_ px: Int) -> Data {
    let s = CGFloat(px)
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px, bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                               bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    // the macOS icon grid: an 824/1024 rounded square, centred
    let inset = s * 100 / 1024
    let rect = NSRect(x: inset, y: inset, width: s - 2 * inset, height: s - 2 * inset)
    let shape = NSBezierPath(roundedRect: rect, xRadius: rect.width * 0.225, yRadius: rect.width * 0.225)
    NSGradient(colors: [NSColor(srgbRed: 0.10, green: 0.16, blue: 0.32, alpha: 1),
                        NSColor(srgbRed: 0.16, green: 0.42, blue: 0.78, alpha: 1)])!
        .draw(in: shape, angle: 70)
    // three nodes joined through a hub: clients and servers through the gateway
    let c = NSPoint(x: s / 2, y: s / 2)
    let r = rect.width * 0.27
    let nodes = [90.0, 210.0, 330.0].map { a -> NSPoint in
        let t = a * .pi / 180
        return NSPoint(x: c.x + r * cos(t), y: c.y + r * sin(t))
    }
    NSColor.white.withAlphaComponent(0.85).setStroke()
    for n in nodes {
        let p = NSBezierPath()
        p.move(to: c)
        p.line(to: n)
        p.lineWidth = rect.width * 0.035
        p.lineCapStyle = .round
        p.stroke()
    }
    NSColor.white.setFill()
    for n in nodes {
        let d = rect.width * 0.13
        NSBezierPath(ovalIn: NSRect(x: n.x - d / 2, y: n.y - d / 2, width: d, height: d)).fill()
    }
    let hub = rect.width * 0.2
    NSColor(srgbRed: 0.45, green: 0.85, blue: 0.95, alpha: 1).setFill()
    NSBezierPath(ovalIn: NSRect(x: c.x - hub / 2, y: c.y - hub / 2, width: hub, height: hub)).fill()
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

for (pt, scales) in [(16, [1, 2]), (32, [1, 2]), (128, [1, 2]), (256, [1, 2]), (512, [1, 2])] {
    for k in scales {
        let name = k == 1 ? "icon_\(pt)x\(pt).png" : "icon_\(pt)x\(pt)@2x.png"
        try render(pt * k).write(to: out.appending(path: name))
    }
}
