// The two states of a drawing for the picture swap on slide 6 of
// effects.pptx: graphics reach PPTX as pictures (SPEC §9.4), so a stroke
// change inside a drawing is a swap between two pictures.
// Regenerate with: typst compile --ppi 150 swap.typ state{p}.png
#set page(width: 220pt, height: 80pt, margin: 4pt, fill: white)
#let force(stroke) = align(center + horizon, box(width: 200pt, height: 60pt, {
  place(left + horizon, line(length: 170pt, stroke: stroke))
  place(right + horizon, polygon(fill: stroke.paint, (0pt, -12pt), (24pt, 0pt), (0pt, 12pt)))
}))

#force(1.5pt + black)
#pagebreak()
#force((paint: rgb("#DC2626"), thickness: 5pt, dash: "dashed"))
