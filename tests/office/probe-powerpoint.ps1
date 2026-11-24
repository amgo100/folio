# Opens the PPTX fixtures in PowerPoint through COM. For each deck it
# records how PowerPoint reads every shape and animation effect, exports the
# slides as PNG, saves a round-tripped copy, and renders motion-chain.pptx to
# video, from which it saves frames every 100 ms as BMP for analyse.py.
# Usage:
#   powershell -File tests/office/probe-powerpoint.ps1 -Out <directory> [-SkipVideo]
# -FramesOnly only extracts frames from a video made by an earlier run.
param([Parameter(Mandatory)][string]$Out, [switch]$SkipVideo, [switch]$FramesOnly)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path

if (-not $FramesOnly) {
$pp = New-Object -ComObject PowerPoint.Application
"PowerPoint $($pp.Build)"
$results = [ordered]@{ powerpoint = "$($pp.Build)"; files = [ordered]@{} }

function Get-Deck($pres) {
  $slides = @()
  foreach ($slide in $pres.Slides) {
    $shapes = @()
    foreach ($s in $slide.Shapes) {
      $text = ""
      if ($s.HasTextFrame) { $text = $s.TextFrame2.TextRange.Text }
      $shapes += [ordered]@{ name = $s.Name; id = $s.Id; type = $s.Type; fill = $s.Fill.Type; text = $text }
    }
    $effects = @()
    foreach ($e in $slide.TimeLine.MainSequence) {
      $dir = $null; try { $dir = $e.EffectParameters.Direction } catch {}
      $path = $null; try { if ($e.Behaviors.Count -gt 0 -and $e.Behaviors.Item(1).Type -eq 1) { $path = $e.Behaviors.Item(1).MotionEffect.Path } } catch {}
      $effects += [ordered]@{
        shape = $e.Shape.Name; type = $e.EffectType; exit = $e.Exit; trigger = $e.Timing.TriggerType
        duration = $e.Timing.Duration; delay = $e.Timing.TriggerDelayTime; paragraph = $e.Paragraph
        direction = $dir; display = $e.DisplayName; path = $path
      }
    }
    $slides += [ordered]@{ index = $slide.SlideIndex; shapes = $shapes; effects = $effects }
  }
  ,$slides
}

foreach ($f in Get-ChildItem (Join-Path $here "pptx") -Filter *.pptx) {
  $info = [ordered]@{}
  try {
    # FileName, ReadOnly, Untitled, WithWindow
    $pres = $pp.Presentations.Open($f.FullName, -1, 0, 0)
  } catch {
    $info.open_error = "$($_.Exception.Message)"
    $results.files[$f.BaseName] = $info
    continue
  }
  $info.slides = Get-Deck $pres
  $dir = Join-Path $Out $f.BaseName
  New-Item -ItemType Directory -Force $dir | Out-Null
  foreach ($slide in $pres.Slides) { $slide.Export((Join-Path $dir "slide$($slide.SlideIndex).png"), "PNG", 1280, 720) }
  $copy = Join-Path $Out "$($f.BaseName).roundtrip.pptx"
  $pres.SaveCopyAs($copy)
  if ($f.BaseName -eq "motion-chain" -and -not $SkipVideo) {
    $video = Join-Path $Out "motion-chain.mp4"
    # FileName, UseTimingsAndNarrations, DefaultSlideDuration, VertResolution, FramesPerSecond, Quality
    $pres.CreateVideo($video, -1, 6, 720, 30, 85)
    # The status can still read 0 (none) just after the call; closing the
    # presentation then would cancel the export. Wait for done (3) or failed (4).
    $deadline = (Get-Date).AddMinutes(10)
    while ($pres.CreateVideoStatus -ne 3 -and $pres.CreateVideoStatus -ne 4) {
      if ((Get-Date) -gt $deadline) { throw "video export timed out" }
      Start-Sleep -Milliseconds 500
    }
    $info.video_status = $pres.CreateVideoStatus
    if ($info.video_status -ne 3) { throw "video export failed" }
  }
  $pres.Close()
  # Re-open the round-tripped copy and read it again.
  $pres = $pp.Presentations.Open($copy, -1, 0, 0)
  $info.roundtrip_slides = Get-Deck $pres
  $pres.Close()
  $results.files[$f.BaseName] = $info
}
$results | ConvertTo-Json -Depth 8 | Set-Content -Encoding utf8 (Join-Path $Out "powerpoint.json")
if ($pp.Presentations.Count -eq 0) { $pp.Quit() }
}

# Frames of the motion video, through the Windows media composition API.
$video = Join-Path $Out "motion-chain.mp4"
if (-not $SkipVideo -and (Test-Path $video)) {
  Add-Type -AssemblyName System.Runtime.WindowsRuntime
  Add-Type -AssemblyName System.Drawing
  $asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
      $_.Name -eq "AsTask" -and $_.GetParameters().Count -eq 1 -and
      $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' })[0]
  function Wait-Op($op, [type]$type) {
    $t = $asTask.MakeGenericMethod($type).Invoke($null, @($op)); [void]$t.Wait(-1); $t.Result
  }
  [void][Windows.Storage.StorageFile, Windows.Storage, ContentType = WindowsRuntime]
  [void][Windows.Media.Editing.MediaComposition, Windows.Media.Editing, ContentType = WindowsRuntime]
  [void][Windows.Graphics.Imaging.ImageStream, Windows.Graphics, ContentType = WindowsRuntime]
  $file = Wait-Op ([Windows.Storage.StorageFile]::GetFileFromPathAsync($video)) ([Windows.Storage.StorageFile])
  $clip = Wait-Op ([Windows.Media.Editing.MediaClip]::CreateFromFileAsync($file)) ([Windows.Media.Editing.MediaClip])
  $comp = New-Object Windows.Media.Editing.MediaComposition
  # PowerShell 5.1 cannot call IVector methods on a WinRT object directly.
  [void][System.Collections.Generic.ICollection[Windows.Media.Editing.MediaClip]].GetMethod("Add").Invoke($comp.Clips, @($clip))
  $frames = Join-Path $Out "motion-frames"
  New-Item -ItemType Directory -Force $frames | Out-Null
  $total = [int]$clip.OriginalDuration.TotalMilliseconds
  "video length $total ms"
  for ($ms = 0; $ms -lt $total; $ms += 100) {
    $op = $comp.GetThumbnailAsync([TimeSpan]::FromMilliseconds($ms), 960, 540, [Windows.Media.Editing.VideoFramePrecision]::NearestFrame)
    $img = Wait-Op $op ([Windows.Graphics.Imaging.ImageStream])
    $stream = [System.IO.WindowsRuntimeStreamExtensions]::AsStreamForRead($img)
    $bmp = New-Object System.Drawing.Bitmap $stream
    $bmp.Save((Join-Path $frames ("{0:D6}.bmp" -f $ms)), [System.Drawing.Imaging.ImageFormat]::Bmp)
    $bmp.Dispose(); $stream.Dispose()
  }
}
"wrote $Out"
