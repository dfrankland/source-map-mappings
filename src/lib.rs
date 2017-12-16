#![deny(missing_debug_implementations)]

extern crate vlq;

mod comparators;

use comparators::ComparatorFunction;
use std::marker::PhantomData;
use std::mem;
use std::u32;

#[derive(Debug)]
pub enum Error {
    UnexpectedNegativeNumber,
    UnexpectedlyBigNumber,
    Vlq(vlq::Error),
}

impl From<vlq::Error> for Error {
    fn from(e: vlq::Error) -> Error {
        Error::Vlq(e)
    }
}

#[derive(Debug)]
enum LazilySorted<T, F> {
    Sorted(Vec<T>, PhantomData<F>),
    Unsorted(Vec<T>),
}

impl<T, F> LazilySorted<T, F>
where
    F: comparators::ComparatorFunction<T>,
{
    fn sort(&mut self) {
        let me = mem::replace(self, LazilySorted::Unsorted(vec![]));
        let items = match me {
            LazilySorted::Sorted(items, _) => items,
            LazilySorted::Unsorted(mut items) => {
                items.sort_unstable_by(F::compare);
                items
            }
        };
        mem::replace(self, LazilySorted::Sorted(items, PhantomData));
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Bias {
    LeastUpperBound,
    GreatestLowerBound,
}

impl Default for Bias {
    fn default() -> Bias {
        Bias::GreatestLowerBound
    }
}

#[derive(Debug)]
pub struct Mappings {
    by_generated: LazilySorted<Mapping, comparators::ByGeneratedLocation>,
    by_original: Option<Vec<Mapping>>,
    computed_column_spans: bool,
}

impl Mappings {
    pub fn by_generated_location(&mut self) -> &[Mapping] {
        self.by_generated.sort();
        match self.by_generated {
            LazilySorted::Sorted(ref items, _) => items,
            LazilySorted::Unsorted(_) => unreachable!(),
        }
    }

    pub fn compute_column_spans(&mut self) {
        if self.computed_column_spans {
            return;
        }

        self.by_generated.sort();
        let by_generated = match self.by_generated {
            LazilySorted::Sorted(ref mut items, _) => items,
            LazilySorted::Unsorted(_) => unreachable!(),
        };
        let mut by_generated = by_generated.iter_mut().peekable();

        while let Some(this_mapping) = by_generated.next() {
            if let Some(next_mapping) = by_generated.peek() {
                if this_mapping.generated_line == next_mapping.generated_line {
                    this_mapping.last_generated_column = Some(next_mapping.generated_column);
                }
            }
        }

        self.computed_column_spans = true;
    }

    pub fn by_original_location(&mut self) -> &[Mapping] {
        if let Some(ref by_original) = self.by_original {
            return by_original;
        }

        self.compute_column_spans();

        let by_generated = match self.by_generated {
            LazilySorted::Sorted(ref items, _) => items,
            LazilySorted::Unsorted(_) => unreachable!(),
        };

        let mut by_original: Vec<_> = by_generated
            .iter()
            .filter(|m| m.original.is_some())
            .cloned()
            .collect();
        by_original.sort_by(<comparators::ByOriginalLocation as ComparatorFunction<_>>::compare);
        self.by_original = Some(by_original);
        self.by_original.as_ref().unwrap()
    }

    pub fn original_location_for(
        &mut self,
        generated_line: u32,
        generated_column: u32,
        bias: Bias,
    ) -> Option<&Mapping> {
        let by_generated = self.by_generated_location();

        let position = by_generated.binary_search_by(|m| {
            m.generated_line
                .cmp(&generated_line)
                .then(m.generated_column.cmp(&generated_column))
        });

        match position {
            Ok(idx) => Some(&by_generated[idx]),
            Err(idx) => match bias {
                Bias::LeastUpperBound => by_generated.get(idx),
                Bias::GreatestLowerBound => if idx == 0 {
                    None
                } else {
                    by_generated.get(idx - 1)
                },
            },
        }
    }
}

impl Default for Mappings {
    fn default() -> Mappings {
        Mappings {
            by_generated: LazilySorted::Unsorted(vec![]),
            by_original: None,
            computed_column_spans: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Mapping {
    generated_line: u32,
    generated_column: u32,
    last_generated_column: Option<u32>,
    original: Option<OriginalLocation>,
}

impl Mapping {
    pub fn generated_line(&self) -> u32 {
        self.generated_line
    }

    pub fn generated_column(&self) -> u32 {
        self.generated_column
    }

    pub fn last_generated_column(&self) -> Option<u32> {
        self.last_generated_column
    }

    pub fn original(&self) -> Option<&OriginalLocation> {
        self.original.as_ref()
    }
}

impl Default for Mapping {
    fn default() -> Mapping {
        Mapping {
            generated_line: 0,
            generated_column: 0,
            last_generated_column: None,
            original: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OriginalLocation {
    source: u32,
    original_line: u32,
    original_column: u32,
    name: Option<u32>,
}

#[inline]
fn is_mapping_separator(byte: u8) -> bool {
    byte == b';' || byte == b','
}

#[inline]
fn read_relative_positive_vlq<B>(previous: &mut u32, input: &mut B) -> Result<(), Error>
where
    B: Iterator<Item = u8>,
{
    let decoded = vlq::decode(input)?;
    let (new, overflowed) = (*previous as i64).overflowing_add(decoded);
    if overflowed || new > (u32::MAX as i64) {
        return Err(Error::UnexpectedlyBigNumber);
    }

    if new < 0 {
        return Err(Error::UnexpectedNegativeNumber);
    }

    *previous = new as u32;
    Ok(())
}

pub fn parse_mappings(input: &[u8]) -> Result<Mappings, Error> {
    let mut generated_line = 0;
    let mut generated_column = 0;
    let mut original_line = 0;
    let mut original_column = 0;
    let mut source = 0;
    let mut name = 0;

    let mut mappings = Mappings::default();
    let mut by_generated = vec![];

    let mut input = input.iter().cloned().peekable();

    while let Some(byte) = input.peek().cloned() {
        match byte {
            b';' => {
                generated_line += 1;
                generated_column = 0;
                input.next().unwrap();
            }
            b',' => {
                input.next().unwrap();
            }
            _ => {
                let mut mapping = Mapping::default();
                mapping.generated_line = generated_line;

                // First is a generated column that is always present.
                read_relative_positive_vlq(&mut generated_column, &mut input)?;
                mapping.generated_column = generated_column as u32;

                // Read source, original line, and original column if the
                // mapping has them.
                mapping.original = if input.peek().cloned().map_or(true, is_mapping_separator) {
                    None
                } else {
                    read_relative_positive_vlq(&mut source, &mut input)?;
                    read_relative_positive_vlq(&mut original_line, &mut input)?;
                    read_relative_positive_vlq(&mut original_column, &mut input)?;

                    Some(OriginalLocation {
                        source: source,
                        original_line: original_line,
                        original_column: original_column,
                        name: if input.peek().cloned().map_or(true, is_mapping_separator) {
                            None
                        } else {
                            read_relative_positive_vlq(&mut name, &mut input)?;
                            Some(name)
                        },
                    })
                };

                by_generated.push(mapping);
            }
        }
    }

    mappings.by_generated = LazilySorted::Unsorted(by_generated);
    Ok(mappings)
}