$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$app = Get-Process VivoKsu.App -ErrorAction SilentlyContinue
$hwnd = $app.MainWindowHandle
$root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
$tCond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Text)
$texts = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $tCond)
for ($i = 0; $i -lt $texts.Count; $i++) {
  $n = $texts.Item($i).Current.Name
  if ($n) { Write-Output ("T[{0}] {1}" -f $i, $n) }
}
$bCond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::ProgressBar)
$bars = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $bCond)
for ($i = 0; $i -lt $bars.Count; $i++) {
  try { $v = $bars.Item($i).GetCurrentPattern([System.Windows.Automation.RangeValuePattern]::Pattern).Current.Value } catch { $v = 'x' }
  Write-Output ("BAR[{0}]={1}" -f $i, $v)
}
