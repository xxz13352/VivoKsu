# Verify live progress/speed during payload_dumper extraction. ASCII only.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

Add-Type -Namespace Win -Name Native -MemberDefinition @'
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  public struct RECT { public int Left; public int Top; public int Right; public int Bottom; }
'@

function Str([int[]]$cp) { [string]::new([char[]]$cp) }
$navFirm   = Str 0x56FA,0x4EF6,0x63D0,0x53D6   # gu jian ti qu
$btnRead   = Str 0x8BFB,0x53D6,0x4FE1,0x606F   # du qu xin xi
$btnExtract= Str 0x63D0,0x53D6,0x955C,0x50CF   # ti qu jing xiang
$url = 'https://sysuptxdl.vivo.com.cn/upgrade/oem/files/20260723141715e0a5c89817ca119172a97d35dbdd23af.zip?sign=ada43925ae87299be4000322f4d39910&t=6a9cc96b'
$outDir = 'C:/Users/17254/Desktop/TOOL/VivoKsu 工具/临时'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$app = Get-Process VivoKsu.App -ErrorAction SilentlyContinue
if (-not $app) { throw 'app not running' }
$hwnd = $app.MainWindowHandle
if ($hwnd -eq 0) { throw 'no window handle' }
[Win.Native]::ShowWindow($hwnd, 9) | Out-Null
Start-Sleep -Milliseconds 800
$root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)

function FindByName($parent, [string]$name, $ctrl) {
  $c1 = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $name)
  $c2 = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, $ctrl)
  $and = New-Object System.Windows.Automation.AndCondition($c1, $c2)
  $parent.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $and)
}
function InvokeButton([string]$name) {
  $b = FindByName $root $name ([System.Windows.Automation.ControlType]::Button)
  if (-not $b) { throw "button not found: $name" }
  $b.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
}
function ToggleCheck([string]$name, [bool]$check) {
  $cb = FindByName $root $name ([System.Windows.Automation.ControlType]::CheckBox)
  if (-not $cb) { throw "checkbox not found: $name" }
  $tp = $cb.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
  $on = ($tp.Current.ToggleState -eq [System.Windows.Automation.ToggleState]::On)
  if ($on -ne $check) { $tp.Toggle() }
}
function Save-Shot([string]$path) {
  $r = New-Object Win.Native+RECT
  [Win.Native]::GetWindowRect($hwnd, [ref]$r) | Out-Null
  $w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
  $bmp = New-Object System.Drawing.Bitmap($w, $h)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($r.Left, $r.Top, 0, 0, (New-Object System.Drawing.Size($w, $h)))
  $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $bmp.Dispose()
}

InvokeButton $navFirm
Start-Sleep -Milliseconds 600

$editCond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit)
$edits = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $editCond)
if ($edits.Count -lt 1) { throw 'no edit controls' }
$edits.Item(0).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($url)
Start-Sleep -Milliseconds 300

InvokeButton $btnRead

$deadline = (Get-Date).AddSeconds(40)
$loaded = $false
while ((Get-Date) -lt $deadline) {
  if (FindByName $root 'apusys' ([System.Windows.Automation.ControlType]::CheckBox)) { $loaded = $true; break }
  Start-Sleep -Milliseconds 500
}
if (-not $loaded) { throw 'partitions did not load' }

# The partition list is virtualized, so scroll to the bottom to realize the larger
# partitions (system_ext is ~273 MB compressed -> a 10-20s extraction to watch live).
$list = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants,
  (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::List)))
if (-not $list) { throw 'no list control' }
$sp = $list.GetCurrentPattern([System.Windows.Automation.ScrollPattern]::Pattern)
for ($p = 100; $p -ge 30; $p -= 5) {
  try { $sp.SetScrollPercent([System.Windows.Automation.ScrollPattern]::NoScroll, $p) } catch {}
  Start-Sleep -Milliseconds 400
  if (FindByName $root 'system_ext' ([System.Windows.Automation.ControlType]::CheckBox)) { break }
}

ToggleCheck 'system_ext' $true
Start-Sleep -Milliseconds 300
InvokeButton $btnExtract

$textCond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)
$barCond  = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::ProgressBar)
$rows = New-Object System.Collections.ArrayList
$shot1 = $false; $shot2 = $false
for ($i = 0; $i -lt 120; $i++) {
  $speed = ''
  foreach ($t in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $textCond)) {
    $n = $t.Current.Name
    if ($n -match '/s') { $speed = $n; break }
  }
  $vals = @()
  foreach ($b in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $barCond)) {
    try { $vals += ('{0:F3}' -f $b.GetCurrentPattern([System.Windows.Automation.RangeValuePattern]::Pattern).Current.Value) }
    catch { $vals += 'x' }
  }
  [void]$rows.Add(('{0},{1},{2}' -f (Get-Date -Format 'HH:mm:ss'), ($vals -join '|'), $speed.Replace(',', ';')))
  if (($i -eq 16) -and -not $shot1) { Save-Shot "$outDir/progress-mid.png"; $shot1 = $true }
  if (($i -eq 48) -and -not $shot2) { Save-Shot "$outDir/progress-late.png"; $shot2 = $true }
  Start-Sleep -Milliseconds 500
}
$rows | Set-Content "$outDir/progress-log.csv" -Encoding UTF8
Write-Output 'DONE'
