use scraper::{ElementRef, Html};
use url::Url;

use super::CardigannDefinition;
use super::definition::{CompiledField, CompiledFilter};
use crate::{IndexerError, Release};

impl CardigannDefinition {
    pub(super) fn parse(&self, html: &str, base: &Url) -> Result<Vec<Release>, IndexerError> {
        let document = Html::parse_document(html);
        document
            .select(&self.rows)
            .map(|row| self.release(row, base))
            .collect()
    }

    fn release(&self, row: ElementRef<'_>, base: &Url) -> Result<Release, IndexerError> {
        let title = self.required(row, "title")?;
        // `magnet` só é lido na falta de `download`: lido antes, um magnet
        // obrigatório ausente derrubaria uma linha que já tem link direto.
        let download = match self.extract(row, "download")? {
            Some(value) => value,
            None => self
                .extract(row, "magnet")?
                .ok_or_else(|| self.bad("download"))?,
        };
        let download_url = self.item_url(base, &download, "download", true)?;
        let info_url = self
            .extract(row, "details")?
            .map(|value| self.item_url(base, &value, "details", false))
            .transpose()?;
        let size = parse_size(&self.required(row, "size")?).ok_or_else(|| self.bad("size"))?;
        let category = self.required(row, "category")?;
        let categories = self
            .mappings
            .get(&category)
            .cloned()
            .ok_or_else(|| self.bad("category"))?;
        Ok(Release {
            indexer: self.id.clone(),
            guid: info_url.as_ref().unwrap_or(&download_url).to_string(),
            title,
            download_url,
            info_url,
            size,
            published: None,
            seeders: self.count(row, "seeders")?,
            leechers: self.count(row, "leechers")?,
            grabs: self.count(row, "grabs")?,
            categories,
            tags: Vec::new(),
        })
    }

    fn extract(
        &self,
        row: ElementRef<'_>,
        field: &'static str,
    ) -> Result<Option<String>, IndexerError> {
        self.fields.get(field).map_or(Ok(None), |selector| {
            selector.extract(row).map_err(|()| self.bad(field))
        })
    }

    fn required(&self, row: ElementRef<'_>, field: &'static str) -> Result<String, IndexerError> {
        self.extract(row, field)?.ok_or_else(|| self.bad(field))
    }

    fn count(&self, row: ElementRef<'_>, field: &'static str) -> Result<Option<u32>, IndexerError> {
        self.extract(row, field)?
            .map(|value| value.parse().map_err(|_| self.bad(field)))
            .transpose()
    }

    fn item_url(
        &self,
        base: &Url,
        value: &str,
        field: &'static str,
        magnet: bool,
    ) -> Result<Url, IndexerError> {
        let url = base.join(value).map_err(|_| self.bad(field))?;
        if !(matches!(url.scheme(), "http" | "https") || magnet && url.scheme() == "magnet")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(self.bad(field));
        }
        Ok(url)
    }

    fn bad(&self, field: &'static str) -> IndexerError {
        IndexerError::InvalidRelease {
            indexer: self.id.clone(),
            field,
        }
    }
}

impl CompiledField {
    fn extract(&self, row: ElementRef<'_>) -> Result<Option<String>, ()> {
        let value = if let Some(text) = &self.text {
            Some(text.clone())
        } else {
            self.selector
                .as_ref()
                .and_then(|selector| row.select(selector).next())
                .and_then(|element| {
                    self.attribute.as_ref().map_or_else(
                        || Some(element.text().collect::<String>()),
                        |attribute| element.value().attr(attribute).map(str::to_owned),
                    )
                })
        };
        let Some(mut value) = value
            .filter(|value| !value.trim().is_empty())
            .or_else(|| self.default.clone())
        else {
            return if self.optional { Ok(None) } else { Err(()) };
        };
        for filter in &self.filters {
            value = match filter {
                CompiledFilter::Trim => value.trim().to_owned(),
                CompiledFilter::Replace(from, to) => value.replace(from, to),
                CompiledFilter::Append(suffix) => format!("{value}{suffix}"),
                CompiledFilter::Prepend(prefix) => format!("{prefix}{value}"),
                CompiledFilter::Split(separator, index) => {
                    let parts: Vec<_> = value.split(separator).collect();
                    let offset = if *index < 0 {
                        parts.len().checked_sub(index.unsigned_abs()).ok_or(())?
                    } else {
                        usize::try_from(*index).map_err(|_| ())?
                    };
                    parts.get(offset).ok_or(())?.to_string()
                }
            };
        }
        let value = value.trim().to_owned();
        if value.is_empty() {
            if self.optional { Ok(None) } else { Err(()) }
        } else {
            Ok(Some(value))
        }
    }
}

// Aritmética inteira evita arredondar u64::MAX ou aceitar NaN, expoentes e
// valores negativos. O formato aceito é decimal sem separador de milhar.
fn parse_size(value: &str) -> Option<u64> {
    let compact: String = value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    let boundary = compact
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(compact.len());
    let number = &compact[..boundary];
    let suffix = compact[boundary..].to_ascii_lowercase();
    let factor: u128 = match suffix.as_str() {
        "" | "b" => 1,
        "kb" | "kib" => 1024,
        "mb" | "mib" => 1024 * 1024,
        "gb" | "gib" => 1024 * 1024 * 1024,
        "tb" | "tib" => 1024_u128.pow(4),
        _ => return None,
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 18
    {
        return None;
    }
    let whole = whole.parse::<u128>().ok()?.checked_mul(factor)?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u128>().ok()?.checked_mul(factor)?
            / 10_u128.pow(u32::try_from(fraction.len()).ok()?)
    };
    u64::try_from(whole.checked_add(fraction)?).ok()
}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn tamanhos_fracionarios_sem_overflow_ou_valores_ambiguos() {
        assert_eq!(parse_size("1.5 GiB"), Some(1_610_612_736));
        assert_eq!(parse_size("18446744073709551615 B"), Some(u64::MAX));
        for value in [
            "NaN",
            "-1 GB",
            "1,024 MB",
            "1.2.3 GB",
            "18446744073709551616",
            "9999999999999999999999999999999999999999 TB",
        ] {
            assert_eq!(parse_size(value), None, "{value}");
        }
    }
}
