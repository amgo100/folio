// Fallback pictures for math-a14.pptx, one page per equation. They are red
// and labelled so that a screenshot shows when a viewer took the fallback.
// Regenerate with: typst compile --ppi 150 fallback.typ eq{p}.png
#set page(width: 640pt / 2, height: 100pt / 2, margin: 2pt, fill: rgb("#FEE2E2"))
#set text(fill: rgb("#B91C1C"), size: 9pt)
#let eq(body) = {
  place(top + left, text(5pt)[image fallback])
  align(center + horizon, body)
}

#eq($v = (Delta s)/(Delta t), quad x_1^2 + y_0 + z^n$)
#pagebreak()
#eq($sum_(i=1)^n i = (n(n+1))/2$)
#pagebreak()
#eq($A = mat(a, b; c, d)$)
#pagebreak()
#set page(height: 80pt / 2)
#eq([Kinetic energy is $E_"k" = 1/2 m v^2$ in joules.])
