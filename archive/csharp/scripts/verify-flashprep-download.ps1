# In-app verification of FlashPrep OTA download (ASCII only).
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -Namespace Win -Name Native -MemberDefinition @'
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
'@
function Str([int[]]$cp) { [string]::new([char[]]$cp) }

$navFlash = Str 0x7EBF,0x5237,0x51C6,0x5907   # xian shua zhun bei
$btnQuery = Str 0x67E5,0x8BE2,0x94FE,0x63A5   # cha xun lian jie
$btnDl    = Str 0x4E0B,0x8F7D,0x0020,0x004F,0x0054,0x0041   # xia zai OTA
$btnStop  = Str 0x505C,0x6B62,0x64CD,0x4F5C   # ting zhi cao zuo

$outDir = 'C:/Users/17254/Desktop/TOOL/VivoKsu 工具/临时'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
Get-ChildItem -Path $outDir -Filter '*_ota*' | Remove-Item -Force -ErrorAction SilentlyContinue

$app = Get-Process VivoKsu.App -ErrorAction SilentlyContinue
if (-not $app) { throw 'app not running' }
$hwnd = $app.MainWindowHandle
[Win.Native]::ShowWindow($hwnd, 9) | Out-Null
Start-Sleep -Milliseconds 700
$root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)

function FindButton([string]$name) {
  $c1 = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $name)
  $c2 = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
  $and = New-Object System.Windows.Automation.AndCondition($c1, $c2)
  return $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $and)
}
function ClickEnabled([string]$name, [int]$timeoutSec = 15) {
  $deadline = (Get-Date).AddSeconds($timeoutSec)
  $b = $null
  while ((Get-Date) -lt $deadline) {
    $b = FindButton $name
    if ($b -and $b.Current.IsEnabled) { break }
    Start-Sleep -Milliseconds 300
  }
  if (-not $b -or -not $b.Current.IsEnabled) { throw "button not enabled: $name" }
  $b.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
}
function Set-Edit([int]$idx, [string]$val) {
  $editCond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit)
  $edits = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $editCond)
  $edits.Item($idx).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($val)
}
function Get-EditValue([int]$idx) {
  $editCond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit)
  $edits = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $editCond)
  return $edits.Item($idx).GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).Current.Value
}

ClickEnabled $navFlash
Start-Sleep -Milliseconds 800

Set-Edit 0 'PD2196'
Set-Edit 1 '15.3.15.3.W10.V000L1'
Set-Edit 3 $outDir
Start-Sleep -Milliseconds 400

ClickEnabled $btnQuery
$deadline = (Get-Date).AddSeconds(20)
$ok = $false
while ((Get-Date) -lt $deadline) {
  if ((Get-EditValue 4) -match 'sysuptxdl') { $ok = $true; break }
  Start-Sleep -Milliseconds 500
}
Write-Output ("QUERY_OK=" + $ok)
if (-not $ok) { throw 'query did not return a link' }

ClickEnabled $btnDl
Start-Sleep -Seconds 12
$dlfile = Get-ChildItem -Path $outDir -Filter '*_ota.zip.download' -ErrorAction SilentlyContinue | Select-Object -First 1
$zipfile = Get-ChildItem -Path $outDir -Filter '*_ota.zip' -ErrorAction SilentlyContinue | Select-Object -First 1
if ($dlfile) { Write-Output ("DOWN_FILE_EXISTS=True length=" + $dlfile.Length) } else { Write-Output 'DOWN_FILE_EXISTS=False' }
if ($zipfile) { Write-Output ("ZIP_COMPLETED=True size=" + $zipfile.Length) } else { Write-Output 'ZIP_COMPLETED=False' }
$stop = FindButton $btnStop
if ($stop -and $stop.Current.IsEnabled) { $stop.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke() }
Write-Output 'DONE'
