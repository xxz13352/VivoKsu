$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -Namespace Win -Name Native -MemberDefinition @'
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int n);
'@
function Str([int[]]$cp) { [string]::new([char[]]$cp) }
$navFlash = Str 0x7EBF,0x5237,0x51C6,0x5907
$app = Get-Process VivoKsu.App -ErrorAction SilentlyContinue
$hwnd = $app.MainWindowHandle
[Win.Native]::ShowWindow($hwnd, 9) | Out-Null
Start-Sleep -Milliseconds 500
$root = [System.Windows.Automation.AutomationElement]::FromHandle($hwnd)
$c1 = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $navFlash)
$c2 = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
$and = New-Object System.Windows.Automation.AndCondition($c1, $c2)
$nav = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $and)
$nav.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
Start-Sleep -Milliseconds 800
$btnCond = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
$btns = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants, $btnCond)
for ($i = 0; $i -lt $btns.Count; $i++) {
  $n = $btns.Item($i).Current.Name
  $code = ($n.ToCharArray() | ForEach-Object { '0x{0:X4}' -f [int]$_ }) -join ','
  $en = $btns.Item($i).Current.IsEnabled
  Write-Output ("BTN[{0}] enabled={1} name='{2}' codes={3}" -f $i, $en, $n, $code)
}
