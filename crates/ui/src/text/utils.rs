use gpui::{ImageSource, SharedUri};

const NUMBERED_PREFIXES_1: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const NUMBERED_PREFIXES_2: &str = "abcdefghijklmnopqrstuvwxyz";

const BULLETS: [&str; 5] = ["•", "◦", "▪", "‣", "⁃"];

/// Returns the prefix for a list item.
pub(super) fn list_item_prefix(ix: usize, ordered: bool, depth: usize) -> String {
    if ordered {
        if depth == 0 {
            return format!("{}. ", ix + 1);
        }

        if depth == 1 {
            return format!(
                "{}. ",
                NUMBERED_PREFIXES_1
                    .chars()
                    .nth(ix % NUMBERED_PREFIXES_1.len())
                    .unwrap()
            );
        } else {
            return format!(
                "{}. ",
                NUMBERED_PREFIXES_2
                    .chars()
                    .nth(ix % NUMBERED_PREFIXES_2.len())
                    .unwrap()
            );
        }
    } else {
        let depth = depth.min(BULLETS.len() - 1);
        let bullet = BULLETS[depth];
        return format!("{} ", bullet);
    }
}

/// Converts a document image URL into an [`ImageSource`] without granting
/// implicit filesystem access.
///
/// A real URI (`https:`, `data:`, `file:`) stays URI-backed and is fetched by
/// the HTTP client. A scheme-less value is *not* a URI, so it is handed to the
/// application's [`gpui::AssetSource`] instead: an embedding app can then serve
/// document-relative images from wherever it keeps them, and one that does not
/// implement it simply gets no image — neither case reaches the network.
pub(super) fn image_source(url: &SharedUri) -> ImageSource {
    // `From<String>` is what draws the line: it keeps a parseable URL as
    // `Resource::Uri` and turns anything else into `Resource::Embedded`.
    // Going through `From<SharedUri>` instead would force every value onto
    // the network path, scheme or not.
    ImageSource::from(url.as_ref().to_string())
}

#[cfg(test)]
mod tests {
    use gpui::{ImageSource, Resource};

    use crate::text::utils::{image_source, list_item_prefix};

    #[test]
    fn test_image_source() {
        fn source(url: &str) -> Resource {
            match image_source(&url.to_string().into()) {
                ImageSource::Resource(resource) => resource,
                _ => panic!("expected a resource for {url:?}"),
            }
        }
        fn assert_uri(url: &str) {
            match source(url) {
                Resource::Uri(uri) => assert_eq!(uri.as_ref(), url),
                other => panic!("expected Uri for {url:?}, got {other:?}"),
            }
        }
        // A value carrying a scheme is fetched as a URI.
        fn assert_embedded(url: &str) {
            match source(url) {
                Resource::Embedded(path) => assert_eq!(path.as_ref(), url),
                other => panic!("expected Embedded for {url:?}, got {other:?}"),
            }
        }
        assert_uri("https://example.com/logo.png");
        assert_uri("http://example.com/logo.png");
        assert_uri("data:image/png;base64,iVBORw0KGgo=");
        assert_uri("file:///absolute/path/logo.svg");

        // Scheme-less values go to the application's asset source instead, so
        // a document-relative image never becomes a network request.
        assert_embedded("website/public/logo.svg");
        assert_embedded("./images/a.png");
        assert_embedded("../images/a.png");
        assert_embedded("/absolute/path/logo.svg");
    }

    #[test]
    fn test_list_item_prefix() {
        assert_eq!(list_item_prefix(0, true, 0), "1. ");
        assert_eq!(list_item_prefix(1, true, 0), "2. ");
        assert_eq!(list_item_prefix(2, true, 0), "3. ");
        assert_eq!(list_item_prefix(10, true, 0), "11. ");
        assert_eq!(list_item_prefix(0, true, 1), "A. ");
        assert_eq!(list_item_prefix(1, true, 1), "B. ");
        assert_eq!(list_item_prefix(2, true, 1), "C. ");
        assert_eq!(list_item_prefix(0, true, 2), "a. ");
        assert_eq!(list_item_prefix(1, true, 2), "b. ");
        assert_eq!(list_item_prefix(6, true, 2), "g. ");
        assert_eq!(list_item_prefix(0, true, 1), "A. ");
        assert_eq!(list_item_prefix(0, true, 2), "a. ");
        assert_eq!(list_item_prefix(0, false, 0), "• ");
        assert_eq!(list_item_prefix(0, false, 1), "◦ ");
        assert_eq!(list_item_prefix(0, false, 2), "▪ ");
        assert_eq!(list_item_prefix(0, false, 3), "‣ ");
        assert_eq!(list_item_prefix(0, false, 4), "⁃ ");
    }
}
