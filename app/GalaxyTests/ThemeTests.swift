import XCTest
@testable import Galaxy

@MainActor
final class ThemeTests: XCTestCase {
    private let light = NSAppearance(named: .aqua)!
    private let dark = NSAppearance(named: .darkAqua)!

    override func tearDown() {
        Theme.select(Theme.systemName, persist: false)
        super.tearDown()
    }

    private func hex(_ color: NSColor, _ appearance: NSAppearance) -> String {
        let c = color.resolved(for: appearance)
        return String(format: "#%02X%02X%02X", Int(round(c.redComponent * 255)),
                      Int(round(c.greenComponent * 255)), Int(round(c.blueComponent * 255)))
    }

    func testListOffersOnlySystemAndTheBuiltInPair() {
        XCTAssertEqual(Theme.names, ["System", "Galaxy Light", "Galaxy Dark"])
    }

    func testSystemFollowsTheAppearanceBetweenTheTwoDefaults() {
        Theme.select(Theme.systemName, persist: false)
        XCTAssertEqual(Theme.definition(for: light).name, "Galaxy Light")
        XCTAssertEqual(Theme.definition(for: dark).name, "Galaxy Dark")
        XCTAssertEqual(hex(Theme.editorBackground, light), "#FFFFFF")
        XCTAssertEqual(hex(Theme.editorBackground, dark), "#15171D")
    }

    func testAChosenThemeWinsOverTheAppearance() {
        Theme.select("Galaxy Dark", persist: false)
        for appearance in [light, dark] {
            XCTAssertEqual(Theme.definition(for: appearance).name, "Galaxy Dark")
            XCTAssertEqual(hex(Theme.editorBackground, appearance), "#15171D")
            XCTAssertEqual(hex(Theme.chrome, appearance), "#1A1D23")
            XCTAssertEqual(hex(Theme.sidebar, appearance), "#1C1F26")
        }
    }

    func testEveryThemeColorsEveryScope() {
        let scopes = ["attribute", "comment", "constant", "constructor", "function", "keyword", "label",
                      "number", "operator", "property", "punctuation", "string", "tag", "type", "variable"]
        for theme in Theme.all {
            for scope in scopes {
                XCTAssertNotNil(theme.syntaxColors[scope], "\(theme.name): \(scope)")
            }
        }
    }

    func testSettingsListsTheSameNames() throws {
        let controller = PreferencesWindowController {}
        let popup = try XCTUnwrap(Mirror(reflecting: controller).children
            .first { $0.label == "appearance" }?.value as? NSPopUpButton)
        XCTAssertEqual(popup.itemTitles, Theme.names)
    }
}
