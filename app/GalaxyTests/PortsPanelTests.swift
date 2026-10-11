import XCTest
@testable import Galaxy

final class PortsPanelTests: XCTestCase {
    func testParsesListeningTCPRowsAndSortsByPort() {
        let output = """
        COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME
        node 42 user 19u IPv6 0x1 0t0 TCP [::1]:3000 (LISTEN)
        python 7 user 4u IPv4 0x2 0t0 TCP 127.0.0.1:8080 (LISTEN)
        """
        XCTAssertEqual(ListeningPortParser.parse(output), [
            ListeningPort(process: "node", pid: 42, address: "[::1]", port: 3000),
            ListeningPort(process: "python", pid: 7, address: "127.0.0.1", port: 8080),
        ])
    }

    func testIgnoresNonListeningRows() {
        let output = "node 42 user 19u IPv4 0x1 0t0 TCP 127.0.0.1:3000->127.0.0.1:4000 (ESTABLISHED)"
        XCTAssertTrue(ListeningPortParser.parse(output).isEmpty)
    }
}
