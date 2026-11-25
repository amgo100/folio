// The PDF figure that the krilla prototype embeds (tests/pdf/README.md).
// typst compile figure.typ figure-1.7.pdf
// typst compile --pdf-standard 2.0 figure.typ figure-2.0.pdf
#set page(width: 220pt, height: 150pt, margin: 10pt)
#set text(font: "Source Sans 3", size: 9pt)
#set document(date: none)
#place(top + left, dx: 20pt, dy: 0pt, line(start: (0pt, 0pt), end: (0pt, 110pt), stroke: 0.8pt))
#place(top + left, dx: 20pt, dy: 110pt, line(start: (0pt, 0pt), end: (175pt, 0pt), stroke: 0.8pt))
#place(top + left, dx: 20pt, dy: 0pt,
  curve(stroke: 1.2pt + blue,
    curve.move((0pt, 110pt)),
    curve.cubic((40pt, 0pt), (100pt, 0pt), (170pt, 90pt))))
#place(top + left, dx: 150pt, dy: 116pt)[$t$ / s]
#place(top + left, dx: 0pt, dy: 0pt)[$v$]
#place(top + left, dx: 70pt, dy: 30pt, text(fill: blue)[projectile])
