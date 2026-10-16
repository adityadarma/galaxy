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

    func testListOffersSystemTheBuiltInPairAndEveryOneDarkProVariant() {
        let names = Theme.names
        XCTAssertEqual(Array(names.prefix(3)), ["System", "Galaxy Light", "Galaxy Dark"])
        for variant in ["One Dark Pro", "One Dark Pro Flat", "One Dark Pro Darker",
                        "One Dark Pro Mix", "One Dark Pro Night Flat"] {
            XCTAssertTrue(names.contains(variant), variant)
        }
        XCTAssertEqual(Set(names).count, names.count, "names are unique")
    }

    func testSystemFollowsTheAppearanceBetweenTheTwoDefaults() {
        Theme.select(Theme.systemName, persist: false)
        XCTAssertEqual(Theme.definition(for: light).name, "Galaxy Light")
        XCTAssertEqual(Theme.definition(for: dark).name, "Galaxy Dark")
        XCTAssertEqual(hex(Theme.editorBackground, light), "#FFFFFF")
        XCTAssertEqual(hex(Theme.editorBackground, dark), "#1E1E1E")
    }

    func testAChosenThemeWinsOverTheAppearance() {
        Theme.select("One Dark Pro Night Flat", persist: false)
        for appearance in [light, dark] {
            XCTAssertEqual(Theme.definition(for: appearance).name, "One Dark Pro Night Flat")
            XCTAssertEqual(hex(Theme.editorBackground, appearance), "#16191D")
            XCTAssertEqual(hex(Theme.chrome, appearance), "#16191D")
            XCTAssertEqual(hex(Theme.sidebar, appearance), "#16191D")
        }
    }

    func testVariantsDifferInSurfacesButShareSyntaxColors() {
        func surfaces(_ name: String) -> [String] {
            Theme.select(name, persist: false)
            return [hex(Theme.editorBackground, dark), hex(Theme.chrome, dark), hex(Theme.sidebar, dark)]
        }
        XCTAssertEqual(surfaces("One Dark Pro"), ["#282C34", "#21252B", "#21252B"])
        XCTAssertEqual(surfaces("One Dark Pro Flat"), ["#282C34", "#282C34", "#282C34"])
        XCTAssertEqual(surfaces("One Dark Pro Darker"), ["#23272E", "#1E2227", "#1E2227"])
        XCTAssertEqual(hex(Theme.syntax("keyword", appearance: dark), dark), "#C678DD")
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
