# Verify the 可视刷写 partition table no longer re-reads on every heartbeat. ASCII only.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

function Str([int[]]$cp) { [string]::new([char[]]$cp) }
$navLine   = Str 0x53EF,0x89C6,0x5237,0x5199   # ke shi shua xie (可视刷写)
$loading   = Str 0x6B63,0x5728,0x8BFB,0x53D6   # zheng zai du qu (正在读取)

$app = Get-Process VivoKsu.App -ErrorAction SilentlyContinue
if (-not $app) { throw 'app not running' }
$hwnd = $app.MainWindowHandle
$root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)

function FindByName($parent, [string]$name, $ctrl) {
  $c1 = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $name)
  $c2 = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, $ctrl)
  $and = New-Object System.Windows.Automation.AndCondition($c1, $c2)
  $parent.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $and)
}
$navBtn = FindByName $root $navLine ([System.Windows.Automation.ControlType]::Button)
if (-not $navBtn) { throw 'nav button not found' }
$navBtn.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()

$textCond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)
function Is-Loading {
  foreach ($t in $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $textCond)) {
    if ($t.Current.Name -like "*$loading*") { return $true }
  }
  return $false
}

# wait up to 15s for the first load to finish (loading text gone)
$deadline = (Get-Date).AddSeconds(15)
while ((Get-Date) -lt $deadline) {
  if (-not (Is-Loading)) { break }
  Start-Sleep -Milliseconds 400
}

# now observe 20s of heartbeats: log whether the loading overlay reappears
$rows = New-Object System.Collections.ArrayList
for ($i = 0; $i -lt 40; $i++) {
  $loadingNow = Is-Loading
  $stamp = Get-Date -Format 'HH:mm:ss'
  [void]$rows.Add(("{0},{1}" -f $stamp, $(if ($loadingNow) { 1 } else { 0 })))
  Start-Sleep -Milliseconds 500
}
$rows | Set-Content 'C:/Users/17254/Desktop/TOOL/VivoKsu 工具/临时/lineread-log.csv' -Encoding UTF8

# count how many times the loading overlay appeared
$appearances = 0
$prev = 0
foreach ($r in $rows) {
  $v = [int]($r.Split(',')[1])
  if ($v -eq 1 -and $prev -eq 0) { $appearances++ }
  $prev = $v
}
Write-Output ("LOADING-APPEARANCES: {0}" -f $appearances)
