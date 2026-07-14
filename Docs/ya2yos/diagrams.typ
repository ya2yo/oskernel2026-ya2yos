// Native Typst diagrams for the Ya2yOS design report.
#let diagram-ink = rgb("303030")
#let diagram-fill = rgb("f6f6f6")
#let diagram-line = rgb("8a8a8a")

#let box(body, width: 82%) = block(
  width: width,
  inset: (x: 10pt, y: 6pt),
  radius: 2pt,
  fill: diagram-fill,
  stroke: diagram-line,
  align(center)[#body],
)

#let flow(items, width: 82%) = align(center)[
  #stack(
    spacing: 3pt,
    ..items.map(item => [#box(item, width: width) #align(center)[#text(fill: diagram-ink)[↓]]]),
  )
]

#let relation(items) = align(center)[
  #table(
    columns: items.len(),
    stroke: diagram-line,
    inset: 7pt,
    ..items.map(item => [#align(center)[#item]]),
  )
]

#let sequence(events) = align(center)[
  #table(
    columns: (1.2fr, 2.5fr, 1.2fr),
    stroke: diagram-line,
    inset: 6pt,
    table.header([*发起者*], [*关键交互*], [*处理对象*]),
    ..events.flatten().map(item => [#item]),
  )
]
