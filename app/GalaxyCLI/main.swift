import AppKit

private let bundleID = "dev.adityadarma.galaxy"

private func usage() -> Never {
    FileHandle.standardError.write(Data("usage: galaxy <file-or-folder>\n".utf8))
    exit(2)
}

private func appURL() -> URL? {
    // The installed command is a symlink to Galaxy.app/Contents/Resources/galaxy.
    // Resolve it first so a development checkout and an installed symlink use
    // the exact app bundle that supplied this helper.
    let executable = URL(fileURLWithPath: CommandLine.arguments[0]).resolvingSymlinksInPath()
    let resources = executable.deletingLastPathComponent()
    let contents = resources.deletingLastPathComponent()
    let bundledApp = contents.deletingLastPathComponent()
    if resources.lastPathComponent == "Resources", contents.lastPathComponent == "Contents",
       bundledApp.pathExtension == "app" {
        return bundledApp
    }
    return NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
}

guard CommandLine.arguments.count == 2 else { usage() }
if ["-h", "--help"].contains(CommandLine.arguments[1]) { usage() }

let path = NSString(string: CommandLine.arguments[1]).expandingTildeInPath
let url = URL(fileURLWithPath: path).standardizedFileURL
var isDirectory: ObjCBool = false
guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory) else {
    FileHandle.standardError.write(Data("galaxy: no such file or directory: \(path)\n".utf8))
    exit(1)
}
guard let app = appURL() else {
    FileHandle.standardError.write(Data("galaxy: Galaxy.app was not found\n".utf8))
    exit(1)
}

let done = DispatchSemaphore(value: 0)
var failure: Error?
let configuration = NSWorkspace.OpenConfiguration()
configuration.activates = true
NSWorkspace.shared.open([url], withApplicationAt: app, configuration: configuration) { _, error in
    failure = error
    done.signal()
}
done.wait()
if let failure {
    FileHandle.standardError.write(Data("galaxy: could not open \(url.path): \(failure.localizedDescription)\n".utf8))
    exit(1)
}
