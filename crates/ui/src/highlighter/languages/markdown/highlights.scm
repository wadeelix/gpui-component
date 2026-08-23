;From nvim-treesitter/nvim-treesitter
(atx_heading (inline) @title)
(setext_heading (paragraph) @title)

[
  (atx_h1_marker)
  (atx_h2_marker)
  (atx_h3_marker)
  (atx_h4_marker)
  (atx_h5_marker)
  (atx_h6_marker)
  (setext_h1_underline)
  (setext_h2_underline)
] @punctuation.special

[
  (link_title)
  (indented_code_block)
  (fenced_code_block)
] @text.literal

[
  (fenced_code_block_delimiter)
] @punctuation.delimiter

(code_fence_content) @none

[
  (link_destination)
] @text.uri

[
  (link_label)
] @text.reference

[
  (list_marker_plus)
  (list_marker_minus)
  (list_marker_star)
  (list_marker_dot)
  (list_marker_parenthesis)
  (thematic_break)
] @punctuation.list_marker

; GFM task list markers: `[ ]` and `[x]`. Captured separately from the bullet
; so an editor can render them as checkboxes rather than as literal brackets.
(task_list_marker_unchecked) @punctuation.list_marker.unchecked
(task_list_marker_checked) @punctuation.list_marker.checked

[
  (block_continuation)
  (block_quote_marker)
] @punctuation.special

[
  (backslash_escape)
] @string.escape