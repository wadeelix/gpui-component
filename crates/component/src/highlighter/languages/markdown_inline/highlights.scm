; Delimiters first: a later pattern does not override an earlier one, and
; `code_span` covers its own backticks — captured after it, they never surface,
; so inline code kept its markup while emphasis melted.
[
  (emphasis_delimiter)
  (code_span_delimiter)
] @punctuation.delimiter

[
  (code_span)
] @text.code.span

((emphasis) @emphasis
  (#set! highlight.allow-overlap))

((strong_emphasis) @emphasis.strong
  (#set! highlight.allow-overlap))

; GFM strikethrough. Without this the `~~` melted away and the word was left
; looking like ordinary prose — the markup gone and nothing standing for it.
((strikethrough) @strikethrough
  (#set! highlight.allow-overlap))

[
  (link_destination)
  (uri_autolink)
] @link_uri

[
  (link_label)
  (link_text)
  (image_description)
] @link_text

; The brackets and parentheses around a link are markup, not part of it. The
; grammar has no node for them, so they are matched as the anonymous tokens
; they are, which is what lets a live preview melt `[text](url)` down to its
; text.
(inline_link
  [
    "["
    "]"
    "("
    ")"
  ] @punctuation.delimiter)

(image
  [
    "!"
    "["
    "]"
    "("
    ")"
  ] @punctuation.delimiter)
