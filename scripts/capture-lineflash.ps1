# Navigate to LineFlash page and capture via CopyFromScreen (screen pixels).
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class NativeWin4 {
    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
    [DllImport("user32.dll")]
    public static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")]
    public static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);
    public struct RECT { public int Left, Top, Right, Bottom; }
}
"@

$ErrorActionPreference = 'Stop'
# Save to an ASCII-only path: this file has no BOM, so Windows PowerShell 5.1
# would misread any Chinese literals as ANSI and the path would be corrupt.
$outPath = Join-Path $env:TEMP "lineflash-screenshot.png"

# "可视刷写"
$label = -join @([char]0x53EF, [char]0x89C6, [char]0x5237, [char]0x5199)

$p = Get-Process VivoKsu.App -ErrorAction Stop | Select-Object -First 1
$hwnd = $p.MainWindowHandle

if ([NativeWin4]::IsIconic($hwnd)) { [NativeWin4]::ShowWindow($hwnd, 9) | Out-Null; Start-Sleep -Milliseconds 400 }
[NativeWin4]::SetForegroundWindow($hwnd) | Out-Null
Start-Sleep -Milliseconds 700

# Navigate to LineFlash
$root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
$nameCond = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::NameProperty, $label)
$btnType = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
    [System.Windows.Automation.ControlType]::Button)
$and = New-Object System.Windows.Automation.AndCondition($btnType, $nameCond)
$btn = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $and)
if ($btn -eq $null) { Write-Output "WARN: LineFlash button not found, capturing current page"; }
else {
    $invoke = $btn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
    $invoke.Invoke()
    Start-Sleep -Milliseconds 1500
}

# Capture from screen
$rect = New-Object NativeWin4+RECT
[NativeWin4]::GetWindowRect($hwnd, [ref]$rect) | Out-Null
$w = $rect.Right - $rect.Left
$h = $rect.Bottom - $rect.Top
Write-Output "RECT: $w x $h at ($($rect.Left),$($rect.Top))"

$bmp = New-Object System.Drawing.Bitmap($w, $h)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($rect.Left, $rect.Top, 0, 0, (New-Object System.Drawing.Size($w, $h)))
$g.Dispose()
$bmp.Save($outPath, [System.Drawing.Imaging.ImageFormat]::Png)
$bmp.Dispose()
Write-Output "Saved: $outPath"
