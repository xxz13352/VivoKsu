$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

# 汉字从 Unicode 码点构造(避免 BOM-less UTF-8 被当 GBK 读乱)
function zh([string]$codes) {
    $sb = New-Object System.Text.StringBuilder
    foreach ($c in ($codes -split ' ')) { if ($c) { [void]$sb.Append([char][Convert]::ToInt32($c, 16)) } }
    return $sb.ToString()
}

$proc = Get-Process VivoKsu.App -ErrorAction SilentlyContinue | Where-Object { $_.MainWindowHandle -ne 0 } | Select-Object -First 1
if (-not $proc) {
    Write-Output 'NOT_RUNNING'
    exit 1
}

$root = [System.Windows.Automation.AutomationElement]::FromHandle($proc.MainWindowHandle)
Write-Output ('WINDOW=' + $root.Current.Name)

# 软件 = 0x8F6F 0x4EF6;find nav button and click it
$softLabel = zh '8F6F 4EF6'
$condition = New-Object System.Windows.Automation.PropertyCondition(
    [System.Windows.Automation.AutomationElement]::NameProperty, $softLabel)
$softButton = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
if (-not $softButton) {
    Write-Output 'SOFT_BUTTON_NOT_FOUND'
    exit 1
}
try {
    $invoke = $softButton.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
    $invoke.Invoke()
}
catch {
    Write-Output 'SOFT_BUTTON_NOT_INVOKABLE'
    exit 1
}
Write-Output 'SOFT_BUTTON_CLICKED'
# 等软件页组件检测(DriverStore 枚举在线程池)完成,再断言
Start-Sleep -Milliseconds 1200

# Dump text, then assert the software page rendered its component rows
$texts = @()
$found = $root.FindAll([System.Windows.Automation.TreeScope]::Descendants,
    [System.Windows.Automation.Condition]::TrueCondition)
foreach ($t in $found) {
    if ($t.Current.ControlType -eq [System.Windows.Automation.ControlType]::Text) {
        $n = $t.Current.Name
        if (-not [string]::IsNullOrWhiteSpace($n)) {
            $texts += $n
            Write-Output ('TXT=' + $n)
        }
    }
}

# 客户端 0x5BA2 0x6237 0x7AEF;手 0x624B 机 0x673A;驱 0x9A71 动 0x52A8;
# 投 0x6295 屏 0x5C4F 工 0x5DE5 具 0x5177;固 0x56FA 件 0x4EF6 解 0x89E3 包 0x5305
$required = @(
    'VivoKsu ' + (zh '5BA2 6237 7AEF'),                                    # VivoKsu 客户端
    (zh '624B 673A') + ' USB ' + (zh '9A71 52A8'),                         # 手机 USB 驱动
    'scrcpy ' + (zh '6295 5C4F 5DE5 5177'),                                # scrcpy 投屏工具
    'payload_dumper ' + (zh '56FA 4EF6 89E3 5305 5DE5 5177')               # payload_dumper 固件解包工具
)
$joined = $texts -join '|'
foreach ($label in $required) {
    if ($joined.IndexOf($label) -lt 0) {
        Write-Output ('MISSING_LABEL=' + $label)
        exit 1
    }
}
Write-Output 'SOFTWARE_PAGE_OK'
exit 0
