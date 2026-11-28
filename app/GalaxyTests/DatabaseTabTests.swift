import SQLite3
import XCTest
@testable import Galaxy

/// SQLite files open in a database tab: tables and rows, in-place edits,
/// added and deleted rows, structure, and a SQL console.
@MainActor
final class DatabaseTabTests: XCTestCase {
    nonisolated(unsafe) private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("galaxy-db-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    @discardableResult
    private func makeDatabase(_ name: String = "app.db") throws -> URL {
        let url = root.appendingPathComponent(name)
        var handle: OpaquePointer?
        XCTAssertEqual(sqlite3_open(url.path, &handle), SQLITE_OK)
        defer { sqlite3_close(handle) }
        let sql = """
            CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, score REAL);
            INSERT INTO users (name, score) VALUES ('ada', 1.5), ('bob', NULL);
            CREATE VIEW scored AS SELECT name FROM users WHERE score IS NOT NULL;
            """
        XCTAssertEqual(sqlite3_exec(handle, sql, nil, nil, nil), SQLITE_OK)
        return url
    }

    private func query(_ url: URL, _ sql: String) throws -> [[SqliteValue]] {
        try SqliteDatabase.open(path: url.path).execute(sql: sql, maxRows: 100).rows
    }

    private func openTab(_ url: URL) throws -> (MainWindowController, DatabaseView) {
        let c = MainWindowController(bridge: CoreBridge())
        c.showWindow(nil)
        try c.openFile(at: url)
        return (c, c.mediaViewer.databaseView)
    }

    func testOnlyRealSQLiteFilesAreDatabases() throws {
        let db = try makeDatabase("data.sqlite")
        XCTAssertEqual(MediaKind.kind(for: db), .database)
        let plain = try makeDatabase("store")
        XCTAssertEqual(MediaKind.kind(for: plain), .database, "the header decides, not the extension")
        let fake = root.appendingPathComponent("fake.db")
        try Data("not sqlite\0".utf8).write(to: fake)
        XCTAssertNil(MediaKind.kind(for: fake))
    }

    func testDatabaseOpensInATabWithTablesAndRows() throws {
        let url = try makeDatabase()
        let (c, view) = try openTab(url)
        defer { c.window?.orderOut(nil) }

        XCTAssertEqual(c.activeTab?.mediaURL, url)
        XCTAssertEqual(c.mediaViewer.kind, .database)
        XCTAssertFalse(view.isHidden)
        XCTAssertEqual(view.objects.map(\.name), ["scored", "users"])
        XCTAssertEqual(view.selectedObject?.name, "users", "a table is shown first")
        XCTAssertEqual(view.dataGrid.rows.count, 2)
        XCTAssertTrue(view.addButton.isEnabled)

        let scored = try XCTUnwrap(view.objects.first { $0.isView })
        view.select(object: scored)
        XCTAssertEqual(view.dataGrid.rows, [[.text(value: "ada")]])
        XCTAssertTrue(view.pageLabel.stringValue.hasSuffix("(read-only)"), "views cannot be edited")
        XCTAssertFalse(view.addButton.isEnabled)
        XCTAssertFalse(view.dataGrid.canEdit(0, 0))
        XCTAssertTrue(c.statusLabel.stringValue.hasPrefix("SQLite database, 1 table"), c.statusLabel.stringValue)
    }

    private func selectUsers(_ view: DatabaseView) throws {
        let users = try XCTUnwrap(view.objects.first { $0.name == "users" })
        view.select(object: users)
        XCTAssertEqual(view.dataGrid.columns, ["id", "name", "score"])
        XCTAssertEqual(view.pageLabel.stringValue, "1–2 of 2")
    }

    func testCellsAreEditedInPlaceAndWrittenToTheFile() throws {
        let url = try makeDatabase()
        let (c, view) = try openTab(url)
        defer { c.window?.orderOut(nil) }
        try selectUsers(view)

        XCTAssertTrue(view.dataGrid.canEdit(1, 1))
        XCTAssertEqual(view.dataGrid.onEdit?(1, 1, "bobby"), .text(value: "bobby"))
        // REAL affinity stores the typed text as a number.
        XCTAssertEqual(view.dataGrid.onEdit?(1, 2, "2.5"), .real(value: 2.5))
        XCTAssertEqual(try query(url, "SELECT name, score FROM users WHERE id = 2"),
                       [[.text(value: "bobby"), .real(value: 2.5)]])

        XCTAssertEqual(view.dataGrid.onEdit?(0, 1, ""), .text(value: ""), "empty text is '', not NULL")
        XCTAssertEqual(try query(url, "SELECT name FROM users WHERE id = 1"), [[.text(value: "")]])
        XCTAssertEqual(view.messageLabel.stringValue, "Saved name.")
    }

    func testTypingInACellWritesTheFullValue() throws {
        let url = try makeDatabase()
        let (c, view) = try openTab(url)
        defer { c.window?.orderOut(nil) }
        try selectUsers(view)
        let table = view.dataGrid.tableView
        view.layoutSubtreeIfNeeded()

        // bob's score is NULL: shown as "NULL", edited from empty text.
        table.selectRowIndexes([1], byExtendingSelection: false)
        table.editColumn(2, row: 1, with: nil, select: true)
        let editor = try XCTUnwrap(c.window?.fieldEditor(false, for: nil))
        XCTAssertEqual(editor.string, "")
        editor.insertText("4")
        c.window?.makeFirstResponder(table)

        XCTAssertEqual(try query(url, "SELECT score FROM users WHERE id = 2"), [[.real(value: 4)]])
        XCTAssertEqual(view.dataGrid.rows[1][2], .real(value: 4))

        // Leaving a cell untouched writes nothing.
        table.editColumn(1, row: 0, with: nil, select: true)
        XCTAssertEqual(c.window?.fieldEditor(false, for: nil)?.string, "ada")
        c.window?.makeFirstResponder(table)
        XCTAssertEqual(view.messageLabel.stringValue, "Saved score.")
    }

    func testAFailedEditShowsSQLitesReason() throws {
        let url = try makeDatabase()
        let (c, view) = try openTab(url)
        defer { c.window?.orderOut(nil) }
        try selectUsers(view)

        XCTAssertNil(view.dataGrid.onEdit?(0, 0, "abc"), "INTEGER PRIMARY KEY rejects text")
        XCTAssertTrue(view.messageLabel.stringValue.contains("mismatch"), view.messageLabel.stringValue)
        XCTAssertEqual(try query(url, "SELECT id FROM users ORDER BY id"),
                       [[.integer(value: 1)], [.integer(value: 2)]])
    }

    func testRowsAreAddedAndDeletedAfterConfirmation() throws {
        let url = try makeDatabase()
        let (c, view) = try openTab(url)
        defer { c.window?.orderOut(nil) }
        try selectUsers(view)

        view.runModal = { alert in
            let grid = alert.accessoryView as? NSGridView
            let field = grid.flatMap { grid in
                (0..<grid.numberOfRows).compactMap { grid.cell(atColumnIndex: 1, rowIndex: $0).contentView as? NSTextField }
                    .first { $0.accessibilityIdentifier() == "database-new-name" }
            }
            field?.stringValue = "cy"
            return .alertFirstButtonReturn
        }
        view.addRow(nil)
        XCTAssertEqual(view.pageLabel.stringValue, "1–3 of 3")
        XCTAssertEqual(try query(url, "SELECT name, score FROM users WHERE id = 3"), [[.text(value: "cy"), .null]])

        view.dataGrid.tableView.selectRowIndexes([0, 2], byExtendingSelection: false)
        view.runModal = { _ in .alertSecondButtonReturn }
        view.deleteSelectedRows(nil)
        XCTAssertEqual(try query(url, "SELECT count(*) FROM users"), [[.integer(value: 3)]], "Cancel keeps the rows")

        var asked = ""
        view.runModal = { alert in
            asked = alert.messageText
            return .alertFirstButtonReturn
        }
        view.deleteSelectedRows(nil)
        XCTAssertEqual(asked, "Delete 2 rows from users?")
        XCTAssertEqual(try query(url, "SELECT name FROM users"), [[.text(value: "bob")]])
        XCTAssertEqual(view.pageLabel.stringValue, "1–1 of 1")
    }

    func testStructureShowsColumnsAndTheCreateStatement() throws {
        let url = try makeDatabase()
        let (c, view) = try openTab(url)
        defer { c.window?.orderOut(nil) }
        try selectUsers(view)

        view.setMode(.structure)
        XCTAssertEqual(view.structureGrid.rows.map { $0[0] }, [.text(value: "id"), .text(value: "name"), .text(value: "score")])
        XCTAssertEqual(view.structureGrid.rows[1][2], .text(value: "yes"), "name is NOT NULL")
        XCTAssertTrue(view.structureText.string.hasPrefix("CREATE TABLE users"), view.structureText.string)
    }

    func testSQLConsoleRunsStatementsAndRefreshesTheTableList() async throws {
        let url = try makeDatabase()
        let (c, view) = try openTab(url)
        defer { c.window?.orderOut(nil) }

        view.setMode(.query)
        view.queryText.string = "CREATE TABLE notes (body TEXT); INSERT INTO notes VALUES ('hi'); SELECT body FROM notes;"
        view.runQuery(nil)
        await view.waitForQuery()

        XCTAssertEqual(view.resultGrid.columns, ["body"])
        XCTAssertEqual(view.resultGrid.rows, [[.text(value: "hi")]])
        XCTAssertEqual(view.messageLabel.stringValue, "1 row, 1 row changed.")
        XCTAssertEqual(view.objects.map(\.name), ["notes", "scored", "users"])

        view.queryText.string = "SELECT * FROM missing"
        view.runQuery(nil)
        await view.waitForQuery()
        XCTAssertTrue(view.messageLabel.stringValue.contains("no such table"), view.messageLabel.stringValue)
    }
}
