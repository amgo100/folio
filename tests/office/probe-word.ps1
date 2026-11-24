# Opens the DOCX fixtures in Word through COM, exports each to XPS (for
# glyph positions) and PDF (for viewing), and records list labels and field
# results before and after a field update. Usage:
#   powershell -File tests/office/probe-word.ps1 -Out <directory>
# The XPS files are read by analyse.py; word.json holds the rest.
param([Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path

$word = New-Object -ComObject Word.Application
$word.Visible = $false
$word.DisplayAlerts = 0
"Word $($word.Version) build $($word.Build)"

function Open-Doc($path) {
  # FileName, ConfirmConversions, ReadOnly, AddToRecentFiles, PasswordDocument,
  # PasswordTemplate, Revert, WritePasswordDocument, WritePasswordTemplate,
  # Format, Encoding, Visible, OpenAndRepair, DocumentDirection, NoEncodingDialog
  $word.Documents.Open($path, $false, $true, $false, "", "", $false, "", "", 0, 0, $false, $false, 0, $true)
}

$results = [ordered]@{ word = "$($word.Build)"; files = [ordered]@{} }
foreach ($f in Get-ChildItem (Join-Path $here "docx") -Filter *.docx) {
  $doc = Open-Doc $f.FullName
  $base = Join-Path $Out $f.BaseName
  $doc.Repaginate()
  $doc.SaveAs2("$base.xps", 18)  # wdFormatXPS
  $doc.SaveAs2("$base.pdf", 17)  # wdFormatPDF
  $info = [ordered]@{ pages = $doc.ComputeStatistics(2) }

  if ($f.BaseName -eq "numbering-gaps") {
    function Get-Labels($doc) {
      $labels = @()
      foreach ($p in $doc.Paragraphs) {
        $t = $p.Range.Text.Trim()
        if ($t -match "[Ee]xpected (\S+)") {
          $labels += [ordered]@{ expected = $Matches[1]; label = $p.Range.ListFormat.ListString; text = $t.Substring(0, [math]::Min(60, $t.Length)) }
        }
      }
      ,$labels
    }
    function Get-Fields($doc) {
      $out = @()
      foreach ($fl in $doc.Fields) { $out += [ordered]@{ code = $fl.Code.Text.Trim(); result = $fl.Result.Text } }
      ,$out
    }
    $info.labels_as_opened = Get-Labels $doc
    $info.fields_as_opened = Get-Fields $doc
    [void]$doc.Fields.Update()
    $info.fields_after_update = Get-Fields $doc
    $info.labels_after_update = Get-Labels $doc
    # Insert a question between Question 2 and Question 4 of list X, as an
    # author would in Word: (a) Enter at the end of Question 2(a), then
    # Shift+Tab, so the new paragraph joins the instance before the gap;
    # (b) Enter at the start of Question 4, so it joins the instance after.
    foreach ($case in "end-of-previous", "start-of-next") {
      $doc.Undo(100) | Out-Null
      foreach ($c in $doc.Paragraphs) {
        if ($c.Range.Text -match "^INSERTED") { throw "undo left an inserted paragraph behind" }
      }
      foreach ($p in $doc.Paragraphs) {
        if ($p.Range.Text -match "INSERTAFTER") {
          $part = $p.Next()
          $q4 = $part.Next()
          # A Paragraph object follows its paragraph mark, so after a split
          # the new paragraph is found again by its text.
          if ($case -eq "end-of-previous") {
            $r = $part.Range; [void]$r.MoveEnd(1, -1); $r.Collapse(0)
            $r.InsertAfter("`rINSERTED")
          } else {
            $r = $q4.Range; $r.Collapse(1)
            $r.InsertBefore("INSERTED`r")
          }
          $new = $null
          foreach ($c in $doc.Paragraphs) { if ($c.Range.Text -match "^INSERTED") { $new = $c; break } }
          if ($case -eq "end-of-previous") { $new.Range.ListFormat.ListOutdent() }
          $info["insert_$case"] = [ordered]@{
            inserted = $new.Range.ListFormat.ListString
            question4 = $new.Next().Range.ListFormat.ListString
            question4_text = $new.Next().Range.Text.Trim().Substring(0, 12)
          }
          break
        }
      }
    }
  }
  $results.files[$f.BaseName] = $info
  $doc.Close(0)
}
$results | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 (Join-Path $Out "word.json")
if ($word.Documents.Count -eq 0) { $word.Quit() }
"wrote $Out"
