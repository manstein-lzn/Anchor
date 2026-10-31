#let report = json(sys.inputs.report)
#let accent = rgb("#137c77")
#let ink = rgb("#1e3542")
#let muted = rgb("#6b7f8b")
#let rule = rgb("#cbd7db")
#let pad-number(value) = if value < 10 { "0" + str(value) } else { str(value) }
#let page-size = (width: 210mm, height: 297mm)
#let page-margins = (left: 49pt, right: 49pt, top: 68pt, bottom: 58pt)

#set document(
  title: "AI 每日晨报 · " + report.issue_date,
  author: "AI 每日晨报",
)
#set text(
  font: ("DejaVu Sans", "Noto Sans CJK SC"),
  size: 11.2pt,
  fill: ink,
  lang: "zh",
  hyphenate: false,
  overhang: false,
  tracking: 0pt,
)
#set par(justify: false, leading: 8pt, spacing: 5pt)
#set page(
  ..page-size,
  margin: page-margins,
  header: [
    #set text(size: 8pt)
    #grid(
      columns: (22pt, 1fr, auto),
      column-gutter: 9pt,
      align: (left, left, right),
      align(horizon, rect(width: 22pt, height: 2.5pt, fill: accent, stroke: none)),
      text(fill: accent, weight: "bold", "AI DAILY BRIEF"),
      text(fill: muted, report.issue_label + "  /  " + report.weekday),
    )
    #v(10pt)
    #line(length: 100%, stroke: (paint: ink, thickness: 0.5pt))
  ],
  header-ascent: 19pt,
  footer: [
    #line(length: 100%, stroke: (paint: rule, thickness: 0.45pt))
    #v(8pt)
    #grid(
      columns: (1fr, auto),
      align: (left, right),
      text(size: 8pt, fill: muted, "AI 每日晨报"),
      context text(
        size: 8pt,
        weight: "bold",
        fill: muted,
        pad-number(counter(page).get().first()) + " / " + pad-number(counter(page).final().first()),
      ),
    )
  ],
  footer-descent: 9pt,
)
#show link: it => text(size: 8.5pt, fill: accent, underline(it))

#text(size: 28pt, weight: "bold", "AI 每日晨报")
#v(9pt)
#text(size: 8.5pt, fill: muted, "观察窗口：" + report.window_label)
#linebreak()
#text(size: 8.5pt, fill: muted, "采集截至：" + report.cutoff_label + "（北京时间）")
#v(10pt)
#line(length: 100%, stroke: (paint: rule, thickness: 0.45pt))
#v(9pt)
#if report.intro.len() > 0 { block(
  width: 100%,
  inset: (x: 14pt, y: 11pt),
  fill: rgb("#edf5f3"),
  breakable: false,
)[
  #text(size: 8.5pt, weight: "bold", fill: accent, "本期速读")
  #v(6pt)
  #set text(size: 10.5pt)
  #set par(leading: 10pt, spacing: 10pt)
  #for (index, item) in report.intro.enumerate() {
    text(item)
    if index < report.intro.len() - 1 { parbreak() }
  }
] }
#v(15pt)

#for (index, section) in report.sections.enumerate() {
  [
    #set par(leading: 7pt, spacing: 5pt)
    #block(breakable: false, sticky: true)[
      #grid(
        columns: (1fr, auto),
        gutter: 8pt,
        align: (left, right),
        [#text(size: 8.5pt, fill: accent, weight: "bold", pad-number(index + 1)) #h(8pt) #text(size: 8.5pt, fill: accent, section.category)],
        text(size: 8pt, fill: muted, section.date_label),
      )
      #v(5pt)
      #block[
        #set par(leading: 5pt)
        // Anchor the inline start so CJK punctuation stays within the page margin.
        #box(width: 0pt)#text(size: 16pt, section.title)
      ]
    ]
    #v(7pt)
    #block(inset: (x: 6pt))[
      #set par(leading: 8pt, spacing: 8pt)
      #for (paragraph_index, paragraph) in section.paragraphs.enumerate() {
        text(paragraph)
        if paragraph_index < section.paragraphs.len() - 1 { parbreak() }
      }
    ]
    #v(7pt)
    #block(inset: (x: 6pt))[
      #set par(leading: 4pt)
      #text(size: 8.5pt, fill: accent, "来源：")
      #for (source_index, source) in section.sources.enumerate() {
        if source_index > 0 { text(size: 8.5pt, fill: accent, " / ") }
        link(source.url, source.title)
      }
    ]
    #v(10pt)
    #line(length: 100%, stroke: (paint: rule, thickness: 0.45pt))
  ]
  v(10pt)
}

#if report.at("coverage_note", default: none) != none {
  block(width: 100%, breakable: false)[
    #text(size: 8.5pt, fill: muted, report.coverage_note)
  ]
}
