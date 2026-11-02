param([int]$ProcessId)
$ErrorActionPreference='Stop'
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class PuTTYWindowProbe {
  private delegate bool Callback(IntPtr w,IntPtr l);
  [DllImport("user32.dll")] private static extern bool EnumWindows(Callback c,IntPtr l);
  [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr w,out uint p);
  [DllImport("user32.dll",SetLastError=true)] private static extern IntPtr SendMessageTimeout(IntPtr w,uint m,UIntPtr wp,IntPtr lp,uint f,uint t,out UIntPtr r);
  public static IntPtr Window(uint pid) { IntPtr found=IntPtr.Zero; EnumWindows((w,l)=>{ uint p; GetWindowThreadProcessId(w,out p); if(p==pid) {found=w; return false;} return true; },IntPtr.Zero); return found; }
  public static bool Ping(IntPtr w) { UIntPtr r; return SendMessageTimeout(w,0,UIntPtr.Zero,IntPtr.Zero,2,2000,out r)!=IntPtr.Zero; }
}
'@
$window=[IntPtr]::Zero
for($attempt=0;$attempt -lt 100;$attempt++) {
    $window=[PuTTYWindowProbe]::Window([uint32]$ProcessId)
    if($window -ne [IntPtr]::Zero) { break }
    Start-Sleep -Milliseconds 20
}
if($window -eq [IntPtr]::Zero) { throw 'PuTTY test window not found' }
$measured=Get-Process -Id $ProcessId
$cpuBefore=$measured.TotalProcessorTime.TotalSeconds
$elapsed=[Diagnostics.Stopwatch]::StartNew()
$latencies=@()
for($sample=0;$sample -lt 100;$sample++) {
    $watch=[Diagnostics.Stopwatch]::StartNew()
    if(![PuTTYWindowProbe]::Ping($window)) { throw 'PuTTY event loop did not respond within 2 seconds' }
    $latencies+= $watch.Elapsed.TotalMilliseconds
    Start-Sleep -Milliseconds 20
}
$measured.Refresh()
$ordered=@($latencies | Sort-Object)
[ordered]@{
    elapsed_seconds=$elapsed.Elapsed.TotalSeconds
    cpu_seconds=$measured.TotalProcessorTime.TotalSeconds-$cpuBefore
    peak_working_set_bytes=$measured.PeakWorkingSet64
    event_loop_ping_p50_ms=$ordered[49]
    event_loop_ping_p95_ms=$ordered[94]
    event_loop_ping_max_ms=$ordered[99]
    samples=100
} | ConvertTo-Json -Compress

