//! The app against the real `lucina-notebooks` engine, over real HTTP on loopback, through the real client. Only the model, the
//! embeddings, Keep and speech are stand-ins (apps/lucina-notebooks-sample).

use std::sync::atomic::Ordering;
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use lucina_account::Client;
use lucina_app::keys::{self, Mods};
use lucina_app::{Harness, time::Zone};
use lucina_notebooks_sample::{Sample, serve};
use serde_json::json;

use crate::app::{AddMode, Link, Notebooks, Sheet, SheetKind, Tab, Todo};
use crate::server::{Answer, Failure, Job, Msg, Tag, api, handle};

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    })
}

struct Rig {
    h: Harness<Notebooks>,
    rx: mpsc::Receiver<Job>,
    client: Client,
    sample: Sample,
}

impl Rig {
    fn seeded() -> Rig {
        Rig::with(true, 1920, 1080)
    }

    fn with(seed: bool, w: u32, h: u32) -> Rig {
        let sample = runtime().block_on(serve(0, Duration::ZERO, seed)).unwrap();
        let client = Client::new(&sample.base, None).unwrap();
        let (jobs, rx) = mpsc::channel::<Job>();
        let link: Link = Box::new(move |j| {
            let _ = jobs.send(j);
        });
        let h = Harness::new(Notebooks::new(Zone::utc(), Some(link)), w, h);
        let mut r = Rig {
            h,
            rx,
            client,
            sample,
        };
        r.pump();
        r
    }

    /// What the worker threads do, here on this thread. An event with no effect makes `view` run, as a timer's wake does.
    fn pump(&mut self) {
        for _ in 0..12 {
            self.h.core.input(lucina_app::Input::Wheel(0));
            self.h.core.paint();
            let mut any = false;
            while let Ok(job) = self.rx.try_recv() {
                any = true;
                let mut out = Vec::new();
                handle(&self.client, job, &mut |m| out.push(m));
                for m in out {
                    self.h.core.message(m);
                }
            }
            self.h.core.paint();
            if !any {
                return;
            }
        }
    }

    /// Lets the poll fire, and waits for the engine's workers, until `done` says so.
    fn wait_for(&mut self, mut done: impl FnMut(&mut Rig) -> bool) {
        let start = Instant::now();
        loop {
            self.pump();
            if done(self) {
                return;
            }
            if start.elapsed() > Duration::from_secs(40) {
                let list = format!("{:?}", self.app().current_load().is_loading());
                let notice = self.app().page.as_ref().map(|p| {
                    (
                        p.notice.clone(),
                        p.chat.error.clone(),
                        p.detail.is_loading(),
                    )
                });
                panic!("timed out; list loading {list}, page {notice:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn app(&mut self) -> &mut Notebooks {
        self.h.app()
    }

    fn key(&mut self, code: u16) {
        self.h.key(code);
        self.pump();
    }

    fn open_first(&mut self) {
        self.wait_for(|r| r.app().current_list().is_some_and(|l| !l.is_empty()));
        assert!(self.app().list.selected.is_some());
        // Notebooks updated in the same second may come in either order.
        let at = self
            .app()
            .current_list()
            .unwrap()
            .iter()
            .position(|n| n.name == "Network security")
            .unwrap_or(0);
        self.app().list.select(at);
        self.key(keys::KEY_ENTER);
        self.wait_for(|r| r.detail_ready());
    }

    fn detail_ready(&mut self) -> bool {
        self.app()
            .page
            .as_ref()
            .is_some_and(|p| p.detail.value().is_some() && !p.detail.is_loading())
    }

    fn apply(&mut self, todo: Todo) {
        self.app().apply(todo);
        self.pump();
    }

    fn typed(&mut self, s: &str) {
        self.h.type_str(s);
        self.pump();
    }

    fn page(&mut self) -> &mut crate::app::Page {
        self.app().page.as_mut().expect("a notebook is open")
    }
}

fn first_detail(r: &mut Rig) -> crate::wire::Detail {
    r.page().detail.value().cloned().unwrap()
}

#[test]
fn the_list_comes_from_the_engine_and_enter_opens_a_notebook() {
    let mut r = Rig::seeded();
    r.wait_for(|r| r.app().current_list().is_some());
    let names: Vec<String> = r
        .app()
        .current_list()
        .unwrap()
        .iter()
        .map(|n| n.name.clone())
        .collect();
    assert_eq!(
        names.len(),
        2,
        "the archived one is not in the active list: {names:?}"
    );
    assert!(names.contains(&"Network security".to_owned()));
    let at = names.iter().position(|n| n == "Network security").unwrap();
    r.app().list.select(at);
    r.key(keys::KEY_ENTER);
    r.wait_for(|r| r.detail_ready());
    let d = first_detail(&mut r);
    assert_eq!(d.sources.len(), 3);
    assert_eq!(d.notes.len(), 2);
    assert_eq!(d.notebook.counts.sources, 3);
    assert_eq!(r.page().tab, Tab::Sources);
    // Esc goes back, and the list asks again.
    r.key(keys::KEY_ESC);
    assert!(r.app().page.is_none());
}

#[test]
fn archived_notebooks_are_their_own_list() {
    let mut r = Rig::seeded();
    r.wait_for(|r| r.app().current_list().is_some());
    r.apply(Todo::Filter(true));
    r.wait_for(|r| r.app().current_load().value().is_some());
    let l = r.app().current_list().unwrap().clone();
    assert_eq!(l.len(), 1);
    assert_eq!(l[0].name, "Last year's course");
    assert!(l[0].archived);
}

#[test]
fn a_new_notebook_typed_at_the_keyboard_opens_empty() {
    let mut r = Rig::with(false, 800, 600);
    r.wait_for(|r| r.app().current_list().is_some_and(Vec::is_empty));
    r.apply(Todo::NewNotebook);
    r.typed("Biology");
    r.key(keys::KEY_ENTER);
    r.wait_for(|r| r.detail_ready());
    let d = first_detail(&mut r);
    assert_eq!(d.notebook.name, "Biology");
    assert!(d.sources.is_empty());
    assert!(r.app().new_form.is_none());
    assert!(
        r.page().chat.active.is_none(),
        "no chat yet: the first question makes one"
    );
}

#[test]
fn an_empty_name_is_not_sent() {
    let mut r = Rig::with(false, 800, 600);
    r.wait_for(|r| r.app().current_list().is_some());
    r.apply(Todo::NewNotebook);
    let before = r.sample.faults.requests.load(Ordering::SeqCst);
    r.key(keys::KEY_ENTER);
    assert_eq!(r.sample.faults.requests.load(Ordering::SeqCst), before);
    assert!(r.app().new_form.is_some());
}

#[test]
fn a_question_streams_in_and_the_engine_checks_its_citations() {
    let mut r = Rig::seeded();
    r.open_first();
    r.wait_for(|r| !r.page().chat.loading);
    let earlier = r.page().chat.messages.len();
    r.apply(Todo::Tab(Tab::Chat));
    r.typed("What does snooping drop?");
    assert_eq!(r.page().chat.draft.text, "What does snooping drop?");
    r.key(keys::KEY_ENTER);
    r.wait_for(|r| !r.page().chat.sending());
    assert_eq!(r.page().chat.draft.text, "", "Enter sent it");
    let c = &r.page().chat;
    assert_eq!(c.messages.len(), earlier + 2);
    let reply = c.messages.last().unwrap();
    assert_eq!(reply.message.role, "assistant");
    assert!(!reply.cited.is_empty(), "{:?}", reply.message);
    assert!(
        reply.message.text.contains("[source:"),
        "the wire keeps the ids"
    );
    let shown = format!("{:?}", reply.doc.blocks);
    assert!(
        shown.contains("[1]") && !shown.contains("[source:"),
        "the screen shows numbers: {shown}"
    );
    assert_eq!(reply.cited[0].kind, "source");
    assert!(reply.cited[0].source_id.is_some());
}

#[test]
fn shift_enter_is_a_new_line_and_not_a_send() {
    let mut r = Rig::seeded();
    r.open_first();
    r.apply(Todo::Tab(Tab::Chat));
    r.typed("one");
    r.h.press(
        keys::KEY_ENTER,
        Mods {
            shift: true,
            ..Mods::default()
        },
    );
    r.typed("two");
    assert_eq!(r.page().chat.draft.text, "one\ntwo");
    assert!(!r.page().chat.sending());
}

#[test]
fn the_first_question_makes_the_chat_named_after_it() {
    let mut r = Rig::seeded();
    r.open_first();
    r.apply(Todo::NewChat);
    assert!(r.page().chat.active.is_none());
    r.apply(Todo::Tab(Tab::Chat));
    r.typed("How do rate limits work?");
    r.key(keys::KEY_ENTER);
    r.wait_for(|r| !r.page().chat.sending());
    let c = &r.page().chat;
    assert!(c.active.is_some());
    assert_eq!(c.shown_title(), "How do rate limits work?");
    assert_eq!(c.messages.len(), 2);
    assert_eq!(c.chats.len(), 2, "the seeded chat and this one");
}

#[test]
fn a_citation_opens_the_reader_at_its_passage() {
    let mut r = Rig::seeded();
    r.open_first();
    r.wait_for(|r| !r.page().chat.messages.is_empty());
    let i = r
        .page()
        .chat
        .messages
        .iter()
        .rposition(|m| !m.is_user())
        .unwrap();
    let cited = r.page().chat.messages[i].cited[0].clone();
    r.h.app().want_focus = Some("log");
    r.pump();
    r.key(keys::KEY_1);
    let reader = r.page().reader.as_ref().expect("the reader opened");
    assert_eq!(Some(reader.id.clone()), cited.source_id);
    assert_eq!(reader.around, cited.chunk);
    r.wait_for(|r| !r.page().reader.as_ref().unwrap().chunks.is_empty());
    let reader = r.page().reader.as_ref().unwrap();
    let at = reader.text.selected.expect("the cited passage is selected");
    assert_eq!(Some(reader.chunks[at].ordinal), cited.chunk);
    assert_eq!(reader.source.as_ref().unwrap().status, "ready");
    r.key(keys::KEY_ESC);
    assert!(
        r.page().reader.is_none(),
        "Esc closes the reader and stays on the notebook"
    );
}

#[test]
fn a_reply_saved_to_a_note_is_linked_to_the_notebook() {
    let mut r = Rig::seeded();
    r.open_first();
    r.wait_for(|r| !r.page().chat.messages.is_empty());
    let before = first_detail(&mut r).notes.len();
    r.h.app().want_focus = Some("log");
    r.pump();
    r.key(31);
    r.wait_for(|r| !r.page().chat.saved.is_empty());
    r.wait_for(|r| r.detail_ready());
    assert_eq!(first_detail(&mut r).notes.len(), before + 1);
    assert!(r.page().chat.error.is_none());
}

#[test]
fn a_second_key_on_a_reply_does_not_make_a_second_note() {
    let mut r = Rig::seeded();
    r.open_first();
    r.wait_for(|r| !r.page().chat.messages.is_empty());
    let before = first_detail(&mut r).notes.len();
    let i = r
        .page()
        .chat
        .messages
        .iter()
        .rposition(|m| !m.is_user())
        .unwrap();
    // Both before any answer has come back.
    r.h.app().apply(Todo::SaveReply(i));
    r.h.app().apply(Todo::SaveReply(i));
    r.pump();
    r.wait_for(|r| !r.page().chat.saved.is_empty());
    r.apply(Todo::SaveReply(i));
    r.apply(Todo::Reload);
    r.wait_for(|r| r.detail_ready());
    assert_eq!(first_detail(&mut r).notes.len(), before + 1);
}

#[test]
fn a_question_whose_answer_was_lost_but_stored_is_not_offered_again() {
    let mut r = Rig::seeded();
    r.open_first();
    r.wait_for(|r| !r.page().chat.messages.is_empty());
    let chat = r.page().chat.active.clone().unwrap();
    let asked = r
        .page()
        .chat
        .messages
        .iter()
        .rev()
        .find(|m| m.is_user())
        .unwrap()
        .message
        .text
        .clone();
    r.apply(Todo::Tab(Tab::Chat));
    r.typed(&asked);
    r.h.key(keys::KEY_ENTER);
    // The server already holds this question and its reply, and the stream broke before this client heard.
    while r.rx.try_recv().is_ok() {}
    r.h.core.message(Msg::Done(
        Tag::Asked {
            chat: Some(chat),
            text: asked.clone(),
        },
        Err(Failure::Offline("The reply ended early. Try again.".into())),
    ));
    assert_eq!(
        r.page().chat.draft.text,
        asked,
        "at first it comes back to the box"
    );
    r.pump();
    r.wait_for(|r| !r.page().chat.loading);
    assert_eq!(
        r.page().chat.draft.text,
        "",
        "the reply is there, so there is nothing to send"
    );
    assert!(r.page().chat.error.is_none());
}

#[test]
fn a_poll_that_fails_does_not_ask_again_by_itself() {
    let mut r = Rig::seeded();
    r.open_first();
    r.apply(Todo::Sheet(SheetKind::Podcast));
    r.wait_for(|r| matches!(&r.page().sheet, Some(Sheet::Podcast(f)) if !f.episodes.is_empty()));
    r.sample.faults.down.store(true, Ordering::SeqCst);
    // Something is being worked on, as far as this client knows.
    let mut d = first_detail(&mut r);
    d.podcasts.push(crate::wire::Podcast {
        id: "p1".into(),
        status: "voicing".into(),
        ..Default::default()
    });
    r.h.core.message(Msg::Done(
        Tag::Detail {
            notebook: d.notebook.id.clone(),
        },
        Ok(Answer::Detail(Box::new(d))),
    ));
    r.apply(Todo::CloseSheet);
    r.apply(Todo::Reload);
    assert!(matches!(
        r.page().detail,
        lucina_app::Load::Failed(_, Some(_))
    ));
    let before = r.sample.faults.requests.load(Ordering::SeqCst);
    std::thread::sleep(crate::app::POLL + Duration::from_millis(200));
    r.pump();
    assert_eq!(
        r.sample.faults.requests.load(Ordering::SeqCst),
        before,
        "it waits for Try again"
    );
    r.sample.faults.down.store(false, Ordering::SeqCst);
    r.apply(Todo::Reload);
    assert!(r.sample.faults.requests.load(Ordering::SeqCst) > before);
}

#[test]
fn a_prefilled_box_is_replaced_by_what_is_typed() {
    let mut r = Rig::seeded();
    r.open_first();
    r.apply(Todo::Sheet(SheetKind::Podcast));
    r.typed("12");
    let Some(Sheet::Podcast(f)) = &r.page().sheet else {
        unreachable!()
    };
    assert_eq!(f.minutes.text, "12", "not 812");
    r.apply(Todo::CloseSheet);
    r.wait_for(|r| !r.page().chat.messages.is_empty());
    r.apply(Todo::OpenRename);
    r.typed("Mine");
    let Some(Sheet::Rename(e, _)) = &r.page().sheet else {
        unreachable!()
    };
    assert_eq!(e.text, "Mine");
}

#[test]
fn a_source_pasted_as_text_is_read_by_the_engines_workers_and_the_page_follows() {
    let mut r = Rig::seeded();
    r.open_first();
    r.apply(Todo::Sheet(SheetKind::Add));
    r.apply(Todo::AddMode(AddMode::Text));
    r.typed("Short title");
    r.key(keys::KEY_TAB);
    r.typed("Passage text the engine reads.");
    r.apply(Todo::SubmitAdd);
    assert!(
        r.page().sheet.is_none(),
        "the sheet closes when it is accepted"
    );
    r.wait_for(|r| {
        let d = r.page().detail.value().cloned().unwrap();
        d.sources
            .iter()
            .any(|s| s.title == "Short title" && s.status == "ready")
    });
    let d = first_detail(&mut r);
    assert_eq!(d.notebook.counts.sources, 4);
}

#[test]
fn a_bad_link_is_not_sent() {
    let mut r = Rig::seeded();
    r.open_first();
    r.apply(Todo::Sheet(SheetKind::Add));
    let before = r.sample.faults.requests.load(Ordering::SeqCst);
    r.typed("not a link");
    r.key(keys::KEY_ENTER);
    assert_eq!(r.sample.faults.requests.load(Ordering::SeqCst), before);
    assert!(r.page().sheet.is_some());
}

#[test]
fn a_failed_source_says_why_and_retry_asks_again() {
    let mut r = Rig::seeded();
    r.open_first();
    let d = first_detail(&mut r);
    let at = d
        .sources
        .iter()
        .position(|s| s.status == "failed")
        .expect("the dead link failed");
    assert!(
        d.sources[at]
            .error
            .as_deref()
            .unwrap_or("")
            .contains("no network"),
        "{:?}",
        d.sources[at]
    );
    let start = r.sample.faults.requests.load(Ordering::SeqCst);
    r.apply(Todo::Retry(at));
    assert!(r.sample.faults.requests.load(Ordering::SeqCst) > start);
    assert!(r.page().notice.is_none());
}

#[test]
fn the_context_button_goes_round_full_summary_off() {
    let mut r = Rig::seeded();
    r.open_first();
    let d = first_detail(&mut r);
    let at = d.sources.iter().position(|s| s.status == "ready").unwrap();
    assert_eq!(d.sources[at].context.as_deref(), Some("full"));
    r.apply(Todo::ToggleContext(at));
    r.wait_for(|r| r.detail_ready());
    let id = d.sources[at].id.clone();
    let now = first_detail(&mut r)
        .sources
        .into_iter()
        .find(|s| s.id == id)
        .unwrap();
    assert_eq!(now.context.as_deref(), Some("summary"));
}

#[test]
fn removing_a_source_needs_a_second_answer_and_cancel_is_first() {
    let mut r = Rig::seeded();
    r.open_first();
    r.apply(Todo::AskRemoveSource(0));
    assert!(r.page().confirm.is_some());
    // The stray Enter lands on Cancel.
    r.key(keys::KEY_ENTER);
    assert!(r.page().confirm.is_none());
    assert_eq!(first_detail(&mut r).sources.len(), 3);
    r.apply(Todo::AskRemoveSource(0));
    r.key(keys::KEY_TAB);
    r.key(keys::KEY_ENTER);
    r.wait_for(|r| r.detail_ready() && r.page().detail.value().unwrap().sources.len() == 2);
}

#[test]
fn the_keyboard_is_not_lost_when_a_dialog_closes() {
    let mut r = Rig::seeded();
    r.open_first();
    assert_eq!(r.page().sources.selected, Some(0));
    r.apply(Todo::AskRemoveSource(0));
    r.key(keys::KEY_ESC);
    assert!(r.page().confirm.is_none());
    r.key(keys::KEY_DOWN);
    assert_eq!(
        r.page().sources.selected,
        Some(1),
        "the arrows move in the list again"
    );
    r.apply(Todo::AskRemoveSource(1));
    r.key(keys::KEY_ENTER);
    r.key(keys::KEY_DOWN);
    assert_eq!(r.page().sources.selected, Some(2), "after Cancel too");
    r.key(keys::KEY_ENTER);
    assert!(r.page().reader.is_some(), "and Enter opens the row");
    r.wait_for(|r| r.page().reader.as_ref().is_some_and(|x| x.source.is_some()));
    r.key(keys::KEY_ESC);
    assert!(r.page().reader.is_none());
    r.key(keys::KEY_UP);
    assert_eq!(
        r.page().sources.selected,
        Some(1),
        "and closing the reader gives it back"
    );
}

#[test]
fn esc_from_a_notebook_gives_the_keyboard_to_the_list() {
    let mut r = Rig::seeded();
    r.open_first();
    r.key(keys::KEY_ESC);
    assert!(r.app().page.is_none());
    r.wait_for(|r| !r.app().current_load().is_loading());
    r.key(keys::KEY_ENTER);
    assert!(
        r.app().page.is_some(),
        "Enter opens the selected notebook again"
    );
}

#[test]
fn closing_a_hit_on_another_tab_leaves_the_keyboard_where_it_can_be_used() {
    let mut r = Rig::with(true, 800, 600);
    r.open_first();
    assert_eq!(first_detail(&mut r).notebook.name, "Network security");
    r.apply(Todo::Tab(Tab::Chat));
    r.wait_for(|r| !r.page().chat.loading);
    r.apply(Todo::Sheet(SheetKind::Search));
    r.typed("snooping");
    r.wait_for(|r| matches!(&r.page().sheet, Some(Sheet::Search(f)) if f.hits.as_ref().is_some_and(|h| h.iter().any(|x| x.kind == "source"))));
    let at = match &r.page().sheet {
        Some(Sheet::Search(f)) => f
            .hits
            .as_ref()
            .unwrap()
            .iter()
            .position(|h| h.kind == "source")
            .unwrap(),
        _ => unreachable!(),
    };
    r.apply(Todo::OpenHit(at));
    r.wait_for(|r| {
        r.page()
            .reader
            .as_ref()
            .is_some_and(|x| !x.chunks.is_empty())
    });
    r.key(keys::KEY_ESC);
    r.typed("still typing");
    assert_eq!(
        r.page().chat.draft.text,
        "still typing",
        "the Chat tab's box has it"
    );
}

#[test]
fn a_transformation_makes_an_insight_that_can_be_saved_and_deleted() {
    let mut r = Rig::seeded();
    r.open_first();
    let d = first_detail(&mut r);
    let at = d.sources.iter().position(|s| s.status == "ready").unwrap();
    r.apply(Todo::OpenSource(at));
    r.wait_for(|r| {
        let reader = r.page().reader.as_ref().unwrap();
        reader.source.is_some() && !reader.transformations.is_empty()
    });
    let chosen = {
        let reader = r.page().reader.as_ref().unwrap();
        reader.transformations[reader.chosen].id.clone()
    };
    assert_eq!(chosen, "t-dense-summary", "the web's default");
    let before = r.page().reader.as_ref().unwrap().insights.len();
    r.apply(Todo::RunTransformation);
    r.wait_for(|r| {
        let reader = r.page().reader.as_ref().unwrap();
        reader.insights.len() == before + 1
            && reader.insights.iter().all(|i| i.insight.status == "ready")
    });
    let notes = first_detail(&mut r).notes.len();
    r.apply(Todo::SaveInsight(0));
    r.wait_for(|r| !r.page().reader.as_ref().unwrap().saved.is_empty());
    r.wait_for(|r| r.page().detail.value().unwrap().notes.len() == notes + 1);
    r.apply(Todo::AskDeleteInsight(0));
    r.key(keys::KEY_TAB);
    r.key(keys::KEY_ENTER);
    r.wait_for(|r| r.page().reader.as_ref().unwrap().insights.len() == before);
}

#[test]
fn search_waits_for_two_letters_and_opens_a_passage() {
    let mut r = Rig::seeded();
    r.open_first();
    r.h.press(
        33,
        Mods {
            ctrl: true,
            ..Mods::default()
        },
    );
    r.pump();
    assert!(
        matches!(r.page().sheet, Some(Sheet::Search(_))),
        "Ctrl+F opens the search"
    );
    let before = r.sample.faults.requests.load(Ordering::SeqCst);
    r.typed("s");
    std::thread::sleep(Duration::from_millis(350));
    r.pump();
    assert_eq!(
        r.sample.faults.requests.load(Ordering::SeqCst),
        before,
        "one letter asks nothing"
    );
    r.typed("nooping");
    r.wait_for(|r| matches!(&r.page().sheet, Some(Sheet::Search(f)) if f.hits.as_ref().is_some_and(|h| !h.is_empty())));
    r.key(keys::KEY_DOWN);
    let first_source = match &r.page().sheet {
        Some(Sheet::Search(f)) => f
            .hits
            .as_ref()
            .unwrap()
            .iter()
            .position(|h| h.kind == "source")
            .unwrap(),
        _ => unreachable!(),
    };
    r.apply(Todo::OpenHit(first_source));
    assert!(r.page().sheet.is_none());
    r.wait_for(|r| {
        r.page()
            .reader
            .as_ref()
            .is_some_and(|x| !x.chunks.is_empty())
    });
    assert!(r.page().reader.as_ref().unwrap().around.is_some());
}

#[test]
fn a_new_note_is_written_to_keep_and_linked() {
    let mut r = Rig::seeded();
    r.open_first();
    let before = first_detail(&mut r).notes.len();
    r.apply(Todo::Sheet(SheetKind::Write));
    r.typed("My new note");
    r.key(keys::KEY_TAB);
    r.typed("With some text.");
    r.apply(Todo::SubmitWrite);
    r.wait_for(|r| r.page().sheet.is_none());
    r.wait_for(|r| r.page().detail.value().unwrap().notes.len() == before + 1);
    let d = first_detail(&mut r);
    assert!(
        d.notes.iter().any(|n| n.slug == "my-new-note"),
        "{:?}",
        d.notes
    );
    let at = d
        .notes
        .iter()
        .position(|n| n.slug == "my-new-note")
        .unwrap();
    r.apply(Todo::Unlink(at));
    r.wait_for(|r| r.page().detail.value().unwrap().notes.len() == before);
}

#[test]
fn a_slug_that_keep_does_not_have_is_refused_with_its_words() {
    let mut r = Rig::seeded();
    r.open_first();
    r.apply(Todo::Sheet(SheetKind::Link));
    r.apply(Todo::SubmitLink("not-a-note".into()));
    r.wait_for(|r| matches!(&r.page().sheet, Some(Sheet::Link(f)) if f.error.is_some()));
    let Some(Sheet::Link(f)) = &r.page().sheet else {
        unreachable!()
    };
    assert!(
        f.error.as_ref().unwrap().contains("not found"),
        "{:?}",
        f.error
    );
    assert!(!f.busy, "the form can be used again");
}

#[test]
fn the_picker_lists_keep_and_leaves_out_what_is_linked() {
    let mut r = Rig::seeded();
    r.open_first();
    r.apply(Todo::Sheet(SheetKind::Link));
    r.wait_for(|r| matches!(&r.page().sheet, Some(Sheet::Link(f)) if f.picker.notes.is_some()));
    let Some(Sheet::Link(f)) = &r.page().sheet else {
        unreachable!()
    };
    let rows = f.picker.rows();
    assert_eq!(
        rows.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
        ["reading-list"],
        "the two linked notes are left out"
    );
    r.key(keys::KEY_DOWN);
    r.key(keys::KEY_ENTER);
    r.wait_for(|r| r.page().sheet.is_none());
    r.wait_for(|r| {
        r.page()
            .detail
            .value()
            .unwrap()
            .notes
            .iter()
            .any(|n| n.slug == "reading-list")
    });
}

#[test]
fn a_typed_slug_can_be_used_as_it_is() {
    let mut p = crate::app::Picker::new(Vec::new());
    p.query.set("some-slug");
    assert_eq!(p.rows()[0].0, "some-slug");
    p.query.set("Not a Slug");
    assert!(p.rows().is_empty());
    assert!(
        !crate::app::is_slug("-x") && !crate::app::is_slug("a--b") && crate::app::is_slug("a-b-1")
    );
}

#[test]
fn chats_are_renamed_and_deleted_and_the_next_one_opens() {
    let mut r = Rig::seeded();
    r.open_first();
    r.wait_for(|r| !r.page().chat.messages.is_empty());
    r.apply(Todo::OpenRename);
    if let Some(Sheet::Rename(e, _)) = &mut r.page().sheet {
        e.set("Renamed");
    }
    r.apply(Todo::SaveRename);
    r.wait_for(|r| r.page().chat.shown_title() == "Renamed");
    r.apply(Todo::AskDeleteChat);
    r.key(keys::KEY_TAB);
    r.key(keys::KEY_ENTER);
    r.wait_for(|r| r.page().chat.chats.is_empty());
    assert!(r.page().chat.active.is_none());
    assert!(r.page().chat.messages.is_empty());
}

#[test]
fn settings_rename_archive_and_delete_with_the_sources() {
    let mut r = Rig::seeded();
    r.open_first();
    r.apply(Todo::Sheet(SheetKind::Settings));
    if let Some(Sheet::Settings(f)) = &mut r.page().sheet {
        f.name.set("Renamed notebook");
    }
    r.apply(Todo::SaveSettings);
    r.wait_for(|r| r.page().sheet.is_none());
    r.wait_for(|r| r.page().detail.value().unwrap().notebook.name == "Renamed notebook");

    r.apply(Todo::Sheet(SheetKind::Settings));
    r.apply(Todo::Archive);
    r.wait_for(|r| r.page().detail.value().unwrap().notebook.archived);

    r.apply(Todo::Sheet(SheetKind::Settings));
    if let Some(Sheet::Settings(f)) = &mut r.page().sheet {
        f.delete_exclusive = true;
    }
    r.apply(Todo::AskDeleteNotebook);
    r.key(keys::KEY_TAB);
    r.key(keys::KEY_ENTER);
    r.wait_for(|r| r.app().page.is_none());
    assert!(
        !r.app()
            .current_list()
            .unwrap()
            .iter()
            .any(|n| n.name == "Renamed notebook")
    );
}

#[test]
fn a_podcast_runs_the_engines_pipeline_and_stops_where_this_machine_has_no_ffmpeg() {
    let mut r = Rig::seeded();
    r.open_first();
    r.apply(Todo::Sheet(SheetKind::Podcast));
    r.wait_for(|r| matches!(&r.page().sheet, Some(Sheet::Podcast(f)) if !f.episodes.is_empty() && !f.speakers.is_empty()));
    r.apply(Todo::CycleEpisode);
    if let Some(Sheet::Podcast(f)) = &mut r.page().sheet {
        assert_eq!(f.episode, 1);
        f.minutes.set("99");
        f.briefing.set("Focus on snooping");
    }
    r.apply(Todo::CreatePodcast);
    r.wait_for(|r| {
        r.page()
            .detail
            .value()
            .is_some_and(|d| !d.podcasts.is_empty())
    });
    r.wait_for(|r| {
        let d = r.page().detail.value().unwrap();
        !crate::wire::podcast_pending(&d.podcasts[0].status)
    });
    let p = first_detail(&mut r).podcasts[0].clone();
    assert!(p.status == "ready" || p.status == "failed", "{p:?}");
    assert!(!p.transcript.is_empty(), "{p:?}");
    r.apply(Todo::AskDeletePodcast(0));
    r.key(keys::KEY_TAB);
    r.key(keys::KEY_ENTER);
    r.wait_for(|r| r.page().detail.value().unwrap().podcasts.is_empty());
}

#[test]
fn a_dead_token_goes_to_sign_in_and_forgets_what_it_showed() {
    let mut r = Rig::seeded();
    r.open_first();
    r.sample.faults.revoked.store(true, Ordering::SeqCst);
    r.apply(Todo::Reload);
    assert!(r.app().page.is_none());
    assert!(r.app().current_list().is_none());
}

#[test]
fn a_server_that_is_down_says_so_and_keeps_what_is_on_screen() {
    let mut r = Rig::seeded();
    r.open_first();
    r.sample.faults.down.store(true, Ordering::SeqCst);
    r.apply(Todo::Reload);
    assert!(matches!(
        r.page().detail,
        lucina_app::Load::Failed(_, Some(_))
    ));
    r.sample.faults.down.store(false, Ordering::SeqCst);
    r.apply(Todo::Reload);
    r.wait_for(|r| r.detail_ready());
    assert!(matches!(r.page().detail, lucina_app::Load::Ready(_)));
}

#[test]
fn a_question_that_fails_comes_back_to_the_box() {
    let mut r = Rig::seeded();
    r.open_first();
    r.apply(Todo::Tab(Tab::Chat));
    r.typed("Will this reach it?");
    r.sample.faults.down.store(true, Ordering::SeqCst);
    r.key(keys::KEY_ENTER);
    r.wait_for(|r| !r.page().chat.sending());
    assert_eq!(r.page().chat.draft.text, "Will this reach it?");
    assert!(r.page().chat.error.is_some());
    r.sample.faults.down.store(false, Ordering::SeqCst);
}

#[test]
fn nothing_is_listening_means_a_message_and_a_way_to_try_again() {
    let client = Client::new("http://127.0.0.1:9", None).unwrap();
    let mut out = Vec::new();
    handle(&client, Job::Call(api::list(false)), &mut |m| out.push(m));
    let [Msg::Done(Tag::List { archived: false }, Err(Failure::Offline(m)))] = out.as_slice()
    else {
        panic!("not offline")
    };
    assert!(!m.is_empty());
}

#[test]
fn a_stream_that_ends_without_its_message_is_an_error_and_not_a_blank_reply() {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = s.read(&mut buf);
        let body = "event: delta\ndata: {\"text\":\"half \"}\n\n";
        let head =
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
        let _ = s.write_all(head.as_bytes());
        let _ = s.write_all(body.as_bytes());
        // Close our side, then drain the request: closing with unread data sends a reset, which can drop the reply.
        let _ = s.shutdown(std::net::Shutdown::Write);
        let _ = s.read_to_end(&mut Vec::new());
    });
    let client = Client::new(&format!("http://127.0.0.1:{port}"), None).unwrap();
    let mut out = Vec::new();
    let job = Job::Ask {
        notebook: "nb".into(),
        chat: Some("c1".into()),
        text: "q".into(),
        model: "deepseek",
    };
    handle(&client, job, &mut |m| out.push(m));
    assert!(matches!(&out[0], Msg::Delta(p) if p == "half "));
    let Msg::Done(Tag::Asked { chat, text }, Err(Failure::Offline(m))) = &out[1] else {
        panic!("no error")
    };
    assert_eq!((chat.as_deref(), text.as_str()), (Some("c1"), "q"));
    assert!(m.contains("ended early"));
}

#[test]
fn a_late_answer_about_a_notebook_that_was_left_is_dropped() {
    let mut r = Rig::seeded();
    r.open_first();
    let d = first_detail(&mut r);
    let other = r
        .app()
        .current_list()
        .unwrap()
        .iter()
        .find(|n| n.id != d.notebook.id)
        .unwrap()
        .id
        .clone();
    r.app().open_notebook(&other);
    // The first notebook's answer arrives after the second was opened.
    r.h.core.message(Msg::Done(
        Tag::Detail {
            notebook: d.notebook.id.clone(),
        },
        Ok(Answer::Detail(Box::new(d.clone()))),
    ));
    r.pump();
    assert_ne!(
        r.page().detail.value().map(|x| x.notebook.id.clone()),
        Some(d.notebook.id)
    );
}

#[test]
fn the_web_helpers_say_the_same() {
    use crate::wire::*;
    assert_eq!(
        origin_label("https://www.Example.com:443/a?b"),
        "example.com"
    );
    assert_eq!(origin_label("upload:RM Class 2.pdf"), "RM Class 2.pdf");
    assert_eq!(origin_label("sandbox:/home/cua/x"), "sandbox:/home/cua/x");
    assert_eq!(clock(3725.2), "1:02:05");
    assert_eq!(clock(65.0), "1:05");
    assert_eq!(chunk_ordinal("source:s0a9be3c112-14"), Some(14));
    assert_eq!(
        ref_source_id("source:s0a9be3c112-14").as_deref(),
        Some("s0a9be3c112")
    );
    assert_eq!(ref_source_id("insight:i1"), None);
    assert_eq!(count(0, "No sources", "source", "sources"), "No sources");
    assert_eq!(count(1, "No sources", "source", "sources"), "1 source");
    assert_eq!(count(3, "No sources", "source", "sources"), "3 sources");
    let p = profiles(Some(
        &json!(["plain", {"id": "duo", "name": "Duo"}, {"name": "Only name"}, {"x": 1}]),
    ));
    assert_eq!(
        p.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
        ["plain", "duo", "Only name"]
    );
    assert_eq!(p[1].name, "Duo");
}

/// Not a check: `LUCINA_RENDER_DIR=<dir> cargo test -p lucina-app-notebooks screens` writes the screens as pictures.
#[test]
fn screens() {
    let Ok(dir) = std::env::var("LUCINA_RENDER_DIR") else {
        return;
    };
    for (w, h, tag) in [(1920u32, 1080u32, "1080p"), (800, 600, "qemu-size")] {
        let mut r = Rig::with(true, w, h);
        let shot = |r: &mut Rig, name: &str| {
            r.pump();
            r.h.core.input(lucina_app::Input::Wheel(0));
            r.h.core.paint();
            r.h.core.input(lucina_app::Input::Wheel(0));
            r.h.core.paint();
            std::fs::write(format!("{dir}/{tag}-{name}.ppm"), r.h.ppm()).unwrap();
        };
        r.wait_for(|r| r.app().current_list().is_some());
        shot(&mut r, "list");
        let at = r
            .app()
            .current_list()
            .unwrap()
            .iter()
            .position(|n| n.name == "Network security")
            .unwrap();
        r.app().list.select(at);
        r.key(keys::KEY_ENTER);
        r.wait_for(|r| r.detail_ready() && !r.page().chat.messages.is_empty());
        shot(&mut r, "notebook");
        r.apply(Todo::Tab(Tab::Chat));
        shot(&mut r, "chat");
        let i = r
            .page()
            .chat
            .messages
            .iter()
            .rposition(|m| !m.is_user())
            .unwrap();
        r.apply(Todo::Cite(i, 0));
        r.wait_for(|r| {
            r.page()
                .reader
                .as_ref()
                .is_some_and(|x| !x.chunks.is_empty() && !x.insights.is_empty())
        });
        shot(&mut r, "reader");
        r.apply(Todo::ReaderPane(true));
        shot(&mut r, "reader-insights");
        r.apply(Todo::CloseReader);
        r.apply(Todo::Sheet(SheetKind::Search));
        r.typed("snooping");
        r.wait_for(|r| matches!(&r.page().sheet, Some(Sheet::Search(f)) if f.hits.is_some()));
        shot(&mut r, "search");
        r.apply(Todo::CloseSheet);
        r.apply(Todo::Sheet(SheetKind::Add));
        shot(&mut r, "add");
        r.apply(Todo::CloseSheet);
        r.apply(Todo::Sheet(SheetKind::Podcast));
        r.wait_for(
            |r| matches!(&r.page().sheet, Some(Sheet::Podcast(f)) if !f.episodes.is_empty()),
        );
        r.apply(Todo::CreatePodcast);
        r.wait_for(|r| {
            r.page().detail.value().is_some_and(|d| {
                d.podcasts
                    .iter()
                    .any(|p| !p.transcript.is_empty() && !crate::wire::podcast_pending(&p.status))
            })
        });
        r.apply(Todo::ToggleTranscript(0));
        shot(&mut r, "podcast");
        r.apply(Todo::CloseSheet);
        r.apply(Todo::Sheet(SheetKind::Settings));
        shot(&mut r, "settings");
    }
}
