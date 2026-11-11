//! The MultiView's plan (ADR 0014): which input goes in each cell, published by `vma-control`
//! beside the render plan (it does not fit in the plan's words); and the titles, rasterised
//! on a thread of their own (`vma-titles`) when they change (a name, the layout, the input on
//! air when the views are titled by name), so that `vma-control` never waits for them. The
//! render thread writes them to the GPU when their version changes.

use std::sync::Arc;

use arc_swap::ArcSwap;
use vma_compositor::multiview::{LabelImage, MultiviewLayout as Layout};
use vma_core::InputId;

use crate::command::{MULTIVIEW_INPUTS, MultiviewLayout, MultiviewSlot};
use crate::state::State;

/// What the render thread reads of the MultiView.
#[derive(Debug, Clone)]
pub(crate) struct MultiviewPlan {
    pub(crate) layout: Layout,
    pub(crate) titles_below: bool,
    /// The input of each input cell (`None`: empty).
    pub(crate) inputs: [Option<InputId>; MULTIVIEW_INPUTS],
}

/// The titles as the render thread reads them.
#[derive(Debug)]
pub(crate) struct Titles {
    /// Line 0: Preview, 1: Program, then the input cells.
    pub(crate) image: LabelImage,
    /// +1 whenever they change.
    pub(crate) version: u64,
}

/// What `vma-control` and `vma-titles` publish for the render thread.
#[derive(Debug)]
pub(crate) struct Multiview {
    pub(crate) plan: ArcSwap<MultiviewPlan>,
    pub(crate) titles: ArcSwap<Titles>,
}

pub(crate) type MultiviewShared = Arc<Multiview>;

/// The MultiView of an empty show (no title yet: they come from `vma-titles`).
pub(crate) fn shared() -> MultiviewShared {
    Arc::new(Multiview {
        plan: ArcSwap::from_pointee(MultiviewPlan {
            layout: Layout::default(),
            titles_below: false,
            inputs: [None; MULTIVIEW_INPUTS],
        }),
        titles: ArcSwap::from_pointee(Titles {
            image: LabelImage::rasterise(&[]),
            version: 0,
        }),
    })
}

fn layout(layout: MultiviewLayout) -> Layout {
    match layout {
        MultiviewLayout::PlusEight => Layout::PlusEight,
        MultiviewLayout::PlusFourteen => Layout::PlusFourteen,
        MultiviewLayout::PlusTwo => Layout::PlusTwo,
        MultiviewLayout::Clean => Layout::Clean,
    }
}

/// The input of each input cell: chosen ones where they are asked, the others in display
/// order (inputs with pictures only), skipping those already placed.
pub fn slot_inputs(state: &State) -> [Option<InputId>; MULTIVIEW_INPUTS] {
    let settings = &state.multiview;
    let count = settings.layout.inputs().min(MULTIVIEW_INPUTS);
    let slot = |n: usize| settings.slots.get(n).copied().unwrap_or_default();
    let chosen = |n: usize| match slot(n) {
        MultiviewSlot::Input(key) => state.inputs.iter().find(|i| i.key == key).map(|i| i.id),
        _ => None,
    };
    let placed: Vec<InputId> = (0..count).filter_map(chosen).collect();
    let mut rest = state
        .inputs
        .iter()
        .filter(|i| i.spec.kind().has_video() && !placed.contains(&i.id))
        .map(|i| i.id);
    let mut cells = [None; MULTIVIEW_INPUTS];
    for (n, cell) in cells.iter_mut().enumerate().take(count) {
        *cell = match slot(n) {
            MultiviewSlot::Input(_) => chosen(n),
            MultiviewSlot::Auto => rest.next(),
            MultiviewSlot::Empty => None,
        };
    }
    cells
}

/// Titles: Preview, Program, then each input cell (`n Name`).
fn titles(state: &State, inputs: &[Option<InputId>]) -> Vec<String> {
    let named = |id: Option<InputId>| {
        let n = state.inputs.iter().position(|i| Some(i.id) == id)?;
        state.inputs.get(n).map(|i| format!("{} {}", n + 1, i.name))
    };
    let view = |label: &str, id: Option<InputId>| {
        if state.multiview.names_on_views
            && let Some(name) = named(id)
        {
            name
        } else {
            label.to_owned()
        }
    };
    let mut titles = vec![
        view("Preview", state.preview),
        view("Program", state.program),
    ];
    titles.extend(inputs.iter().map(|id| named(*id).unwrap_or_default()));
    titles
}

/// `vma-control`'s side: publishes a new plan when the MultiView changes, and new titles to
/// `vma-titles`.
#[derive(Debug)]
pub(crate) struct MultiviewWriter {
    shared: MultiviewShared,
    titles: Vec<String>,
    to_titles: crossbeam_channel::Sender<Vec<String>>,
}

impl MultiviewWriter {
    /// Starts `vma-titles`; it ends when the writer is dropped.
    pub(crate) fn new(shared: MultiviewShared) -> std::io::Result<Self> {
        let (to_titles, titles) = crossbeam_channel::unbounded::<Vec<String>>();
        let published = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("vma-titles".into())
            .spawn(move || {
                let mut version = 0;
                while let Ok(first) = titles.recv() {
                    // Only the newest titles are worth drawing.
                    let lines = titles.try_iter().last().unwrap_or(first);
                    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
                    version += 1;
                    published.titles.store(Arc::new(Titles {
                        image: LabelImage::rasterise(&lines),
                        version,
                    }));
                }
            })?;
        Ok(Self {
            shared,
            titles: Vec::new(),
            to_titles,
        })
    }

    pub(crate) fn publish(&mut self, state: &State) {
        let inputs = slot_inputs(state);
        let titles = titles(state, &inputs);
        if titles != self.titles {
            // Never full (unbounded); the thread is gone only when VMA stops.
            let _ = self.to_titles.send(titles.clone());
            self.titles = titles;
        }
        let current = self.shared.plan.load();
        let plan = MultiviewPlan {
            layout: layout(state.multiview.layout),
            titles_below: state.multiview.titles_below,
            inputs,
        };
        if current.layout != plan.layout
            || current.titles_below != plan.titles_below
            || current.inputs != plan.inputs
        {
            self.shared.plan.store(Arc::new(plan));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Command, InputSpec, MultiviewSettings};
    use crate::state::Format;
    use vma_core::{FrameRate, Resolution, Rgb8};

    fn show() -> State {
        let mut s = State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        });
        for r in 1..=4 {
            s.apply(&Command::AddInput {
                spec: InputSpec::Color {
                    color: Rgb8::new(r, 0, 0),
                },
                name: Some(format!("C{r}")),
            })
            .unwrap();
        }
        s.apply(&Command::AddInput {
            spec: InputSpec::AudioDevice {
                device: crate::command::DeviceId("mic".into()),
            },
            name: Some("Micro".into()),
        })
        .unwrap();
        s
    }

    /// ADR 0014 § 3: inputs in order (pictures only), chosen ones in their cell and not twice.
    #[test]
    fn cells_follow_the_order_or_the_choice() {
        let mut s = show();
        let ids: Vec<InputId> = s.inputs.iter().map(|i| i.id).collect();
        let cells = slot_inputs(&s);
        assert_eq!(
            &cells[..5],
            &[Some(ids[0]), Some(ids[1]), Some(ids[2]), Some(ids[3]), None]
        );
        assert_eq!(cells[8..], [None; 6], "8 cells in the default layout");

        let key3 = s.inputs[2].key;
        s.apply(&Command::SetMultiview {
            settings: MultiviewSettings {
                layout: MultiviewLayout::PlusTwo,
                slots: vec![MultiviewSlot::Input(key3), MultiviewSlot::Auto],
                ..MultiviewSettings::default()
            },
        })
        .unwrap();
        let cells = slot_inputs(&s);
        assert_eq!(&cells[..3], &[Some(ids[2]), Some(ids[0]), None]);

        s.apply(&Command::SetMultiview {
            settings: MultiviewSettings {
                slots: vec![MultiviewSlot::Empty, MultiviewSlot::Input(key3)],
                ..MultiviewSettings::default()
            },
        })
        .unwrap();
        let cells = slot_inputs(&s);
        assert_eq!(
            &cells[..4],
            &[None, Some(ids[2]), Some(ids[0]), Some(ids[1])]
        );
        assert_eq!(
            s.apply(&Command::SetMultiview {
                settings: MultiviewSettings {
                    slots: vec![MultiviewSlot::Auto; 15],
                    ..MultiviewSettings::default()
                },
            }),
            Err(crate::state::Rejection::MultiviewSlots)
        );
    }

    /// Titles are drawn on `vma-titles` only when what they say changes.
    #[test]
    fn titles_are_rasterised_when_they_change() {
        let mut s = show();
        let shared = shared();
        let mut writer = MultiviewWriter::new(Arc::clone(&shared)).unwrap();
        let version = |shared: &MultiviewShared| {
            // Drawn on their own thread: wait for them.
            std::thread::sleep(std::time::Duration::from_millis(300));
            shared.titles.load().version
        };
        writer.publish(&s);
        let first = version(&shared);
        assert_eq!(first, 1);
        writer.publish(&s);
        s.apply(&Command::Cut).unwrap();
        writer.publish(&s);
        assert_eq!(version(&shared), first, "« Program » does not change");
        s.apply(&Command::SetMultiview {
            settings: MultiviewSettings {
                names_on_views: true,
                ..MultiviewSettings::default()
            },
        })
        .unwrap();
        writer.publish(&s);
        let named = version(&shared);
        assert!(named > first);
        assert_eq!(writer.titles[1], "2 C2", "the input on air");
        s.apply(&Command::RenameInput {
            input: s.inputs[0].id,
            name: "Caméra".into(),
        })
        .unwrap();
        writer.publish(&s);
        assert!(version(&shared) > named);
        assert_eq!(writer.titles[2], "1 Caméra");
    }
}
