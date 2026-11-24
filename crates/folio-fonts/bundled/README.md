# Bundled fonts

The font families that come with the compiler (SPEC §2.5). They are compiled
into the binary, so a project that pins the compiler version pins them too
(SPEC §2.4). Each family keeps its licence in `licences/`.

| Family | Files | Version | Licence | Source |
| --- | --- | --- | --- | --- |
| New Computer Modern | `NewCM10-{Regular,Bold,Italic,BoldItalic}.otf` | 7.0.1 | GUST Font License 1.0 (`licences/GUST-FONT-LICENSE.txt`) | `files/fonts/` of the `typst-assets` 0.13.1 crate |
| New Computer Modern Math | `NewCMMath-Regular.otf` | 4.0 | GUST Font License 1.0 | `files/fonts/` of the `typst-assets` 0.13.1 crate |
| Source Serif 4 | `SourceSerif4-{Regular,Bold,It,BoldIt}.otf` | 4.005 | SIL OFL 1.1 (`licences/OFL-SourceSerif4.txt`) | `OTF/` on the `release` branch of `adobe-fonts/source-serif` |
| Source Sans 3 | `SourceSans3-{Regular,Bold,It,BoldIt}.otf` | 3.052 | SIL OFL 1.1 (`licences/OFL-SourceSans3.txt`) | `OTF/` on the `release` branch of `adobe-fonts/source-sans` |
| Source Code Pro | `SourceCodePro-{Regular,Bold,It,BoldIt}.otf` | 2.042 upright, 1.062 italic | SIL OFL 1.1 (`licences/OFL-SourceCodePro.txt`) | `OTF/` on the `release` branch of `adobe-fonts/source-code-pro` |
| STIX Two Text | `STIXTwoText-{Regular,Bold,Italic,BoldItalic}.ttf` | 2.13 | SIL OFL 1.1 (`licences/OFL-STIXTwo.txt`) | The static instances of Google Fonts, from the `@expo-google-fonts/stix-two-text` 0.4.2 npm package |
| STIX Two Math | `STIXTwoMath-Regular.ttf` | 2.12 | SIL OFL 1.1 (`licences/OFL-STIXTwo.txt`) | `ofl/stixtwomath/` of `google/fonts` |

The STIX Two Text files are the static instances that Google Fonts derives
from the upstream variable font, not the static fonts of the upstream
release (`stipub/stixfonts`), which could not be fetched when the fonts were
added. Replacing them with the upstream release files changes output, so it
goes with a compiler version.

Changing, adding or removing a file changes the output of documents that use
it: do it only with a new compiler version, and update the table above and
`NOTICE`.
