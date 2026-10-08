import Darwin
import Foundation

/// Startup probe used by `scripts/measure-app.sh` (M0-07).
///
/// When `RCODE_BENCH=1`, prints `startup_ms=<n>` to stdout once the window is
/// visible. Time is measured from kernel process start, so it includes dyld,
/// Rust static init, and AppKit setup.
enum StartupProbe {
    static var isEnabled: Bool {
        ProcessInfo.processInfo.environment["RCODE_BENCH"] == "1"
    }

    static func reportWindowReady() {
        guard isEnabled, let ms = millisecondsSinceProcessStart() else { return }
        print(String(format: "startup_ms=%.1f", ms))
        fflush(stdout)
    }

    static func millisecondsSinceProcessStart() -> Double? {
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        guard sysctl(&mib, u_int(mib.count), &info, &size, nil, 0) == 0 else { return nil }

        let start = info.kp_proc.p_starttime
        var now = timeval()
        gettimeofday(&now, nil)
        let seconds = Double(now.tv_sec - start.tv_sec)
        let micros = Double(now.tv_usec - start.tv_usec)
        return seconds * 1000 + micros / 1000
    }
}
