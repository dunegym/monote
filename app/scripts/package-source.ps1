# 打包 MoNote 应用源码（不含构建产物），输出到 out/monote-app-src-<日期>.zip
# 用法：在 monote/app 目录执行  powershell -ExecutionPolicy Bypass -File scripts\package-source.ps1
$ErrorActionPreference = 'Stop'

$appDir = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $appDir '..\..'))
$outDir = Join-Path $repoRoot 'out'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd'
$zipPath = Join-Path $outDir "monote-app-src-$stamp.zip"
$stage = Join-Path $env:TEMP "monote-src-stage-$stamp"

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force -Path $stage | Out-Null

# 1) 复制 app 目录（排除构建产物与缓存）
$excludeDirs = @('target', 'node_modules', '.git')
$excludeFiles = @('*.log')
robocopy $appDir (Join-Path $stage 'app') /E /XD $excludeDirs /XF $excludeFiles /NFL /NDL /NJH /NJS /NP | Out-Null
if ($LASTEXITCODE -ge 8) { throw "robocopy 失败: $LASTEXITCODE" }

# 2) 附上需求/设计基线文档，便于独立评审
$ref = Join-Path $stage 'reference'
New-Item -ItemType Directory -Force -Path $ref | Out-Null
Copy-Item (Join-Path $repoRoot 'monote\docs\01-需求分析文档.md') $ref -Force
Copy-Item (Join-Path $repoRoot 'exp02\docs\01-软件设计文档.md') $ref -Force
Copy-Item (Join-Path $repoRoot 'exp02\README.md') (Join-Path $ref '设计README.md') -Force

# 3) 打包
if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zipPath -CompressionLevel Optimal
$size = (Get-Item $zipPath).Length
Write-Host ("已生成: {0} ({1:N2} MB)" -f $zipPath, ($size / 1MB))
Remove-Item $stage -Recurse -Force
