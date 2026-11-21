//! Native operation dispatch and source diagnostics.
use super::*;

impl SessionVm {
    pub fn dispatch(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        locale: &str,
        budget: NativeBudget,
    ) -> Result<NativeDispatch, AdapterError> {
        let _timing = reel_core::profile::Scope::new(reel_core::profile::Phase::HostCall);
        let work = budget.work();
        self.check_ticket(session, ticket)?;
        if let Some(work) = self.standard.dispatch(&mut self.vm, ticket, work)? {
            self.issued = None;
            return Ok(NativeDispatch::Ready { work });
        }
        let p = self.vm.program().package();
        let media_read = match p.symbols[p.natives[ticket.native.index()].symbol.index()].as_str() {
            "reel::audio::position" => Some("reel::audio::position"),
            "reel::video::position" => Some("reel::video::position"),
            _ => None,
        };
        if let Some(symbol) = media_read {
            self.dispatch_media(session, ticket, symbol)?;
            return Ok(NativeDispatch::Ready { work: 1 });
        }
        // Retain the verified program while dispatch mutates the VM. Its immutable
        // symbol storage is shared, so dispatch needs no per-call string copy.
        let program = self.vm.program().clone();
        let p = program.package();
        let symbol = p.symbols[p.natives[ticket.native.index()].symbol.index()].as_str();
        if let Some(progress) = self.resume_reading_point(session, ticket, symbol, budget)? {
            return Ok(progress);
        }
        match symbol {
            "reel::run_flow" => self.dispatch_run_flow(session, ticket, budget),
            "reel::scrollbar" => {
                if work < 5 {
                    return Ok(NativeDispatch::BudgetYield {
                        work: 0,
                        required_work: 5,
                    });
                }
                let args = self.vm.host_arguments(ticket)?.to_vec();
                let origin = self.stage_origin(ticket);
                let value = self.stage.dispatch(
                    &mut self.vm,
                    self.root_owner,
                    symbol,
                    &args,
                    self.read_limit,
                    origin,
                )?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 5 })
            }
            "reel::scrollbar::scroll_by" | "reel::scrollbar::scroll_to" => {
                let args = self.vm.host_arguments(ticket)?;
                let (bar, amount) = (args[0], stage::number(args[1])?);
                let value = self.stage.scrollbar_offset(
                    self.vm.heap_mut(),
                    bar,
                    amount,
                    symbol.ends_with("scroll_by"),
                )?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::page_stack" | "reel::tab_bar" => {
                self.dispatch_pages(session, ticket, symbol, budget)
            }
            name if name.starts_with("reel::page_stack::") => {
                self.dispatch_pages(session, ticket, symbol, budget)
            }
            "reel::tab_bar::observe_label" => self.dispatch_pages(session, ticket, symbol, budget),
            "reel::game::start" => {
                let value = self.vm.host_arguments(ticket)?[0];
                let entry = self.vm.unbound_function(value)?;
                source::validate_entry(self.vm.program(), entry)
                    .map_err(|_| AdapterError::Input("game.start requires an ordinary function with no captures or required parameters"))?;
                self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
                self.issued = None;
                self.start_requested = Some(entry);
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::stage::capture" => {
                self.dispatch_capture(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::stage::transition" => {
                self.dispatch_transition(session, ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::stage::input" => {
                self.dispatch_input_hints(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::window::info"
            | "reel::window::configure"
            | "reel::stage::pointer"
            | "reel::stage::move_pointer"
            | "reel::stage::focus" => {
                self.dispatch_window(ticket, symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::assets::read" => {
                self.dispatch_asset_read(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::saves::can_capture" => self.dispatch_save_availability(ticket, budget),
            "reel::saves::can_jump" | "reel::saves::jump" | "reel::saves::track_history" => {
                self.dispatch_history_save(session, ticket, symbol)
            }
            "reel::saves::save"
            | "reel::saves::load"
            | "reel::saves::copy"
            | "reel::saves::delete"
            | "reel::saves::list"
            | "reel::saves::thumbnail" => self.dispatch_save(session, ticket, symbol, work),
            "reel::persistent::read" | "reel::persistent::write" | "reel::persistent::delete" => {
                self.dispatch_persistent(ticket, symbol, work)
            }
            "reel::game::exit" => {
                self.vm.respond(ticket, HostResponse::Ready(Value::NULL))?;
                self.issued = None;
                self.exit_requested = true;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::linear_gradient" => {
                let args = self.vm.host_arguments(ticket)?.to_vec();
                let value = gradients::create(self.vm.heap_mut(), &args, self.read_limit)?;
                self.stage
                    .types
                    .mark(self.vm.heap_mut(), value, "reel::types::LinearGradient")?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::appearance" => {
                let args = self.vm.host_arguments(ticket)?.to_vec();
                let value = stage::interaction_appearance::create(
                    self.vm.heap_mut(),
                    &args,
                    self.read_limit,
                )?;
                self.stage
                    .types
                    .mark(self.vm.heap_mut(), value, "reel::types::Appearance")?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::image_skin" => {
                let args = self.vm.host_arguments(ticket)?.to_vec();
                let value = stage::image_skin::create(&self.stage, self.vm.heap_mut(), &args)?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::texture" => {
                let path = self.vm.host_arguments(ticket)?[0];
                let value = effects::create_texture(self.vm.heap_mut(), path, self.read_limit)?;
                self.stage
                    .types
                    .mark(self.vm.heap_mut(), value, "reel::types::Texture")?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            name if name.starts_with("reel::texture::") => Err(AdapterError::Input(
                "texture sampling is evaluated inside a compiled effect",
            )),
            "reel::effect" => {
                let args = self.vm.host_arguments(ticket)?.to_vec();
                let function = self.vm.unbound_function(args[0])?;
                let definition =
                    self.stage
                        .effect_products
                        .get(function)
                        .ok_or(AdapterError::Input(
                            "effect function has no compiled definition",
                        ))?;
                let value = effects::create(self.vm.heap_mut(), function, definition, args[1])?;
                self.stage
                    .types
                    .mark(self.vm.heap_mut(), value, "reel::types::Effect")?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::fragment::sample" => Err(AdapterError::Input(
                "fragment sampling is evaluated inside a compiled effect",
            )),
            name if name.starts_with("reel::mask::") => {
                let args = self.vm.host_arguments(ticket)?.to_vec();
                let value = masks::create(self.vm.heap_mut(), &name[12..], &args, self.read_limit)?;
                self.stage
                    .types
                    .mark(self.vm.heap_mut(), value, "reel::types::Mask")?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::rich" => {
                let source = self.vm.host_arguments(ticket)?[0];
                let response = match rich_text::create(self.vm.heap_mut(), source, self.read_limit)
                {
                    Ok(value) => HostResponse::Ready(value),
                    Err(AdapterError::Markup(error)) => {
                        let value = text::alloc_string(
                            self.vm.heap_mut(),
                            &error.to_string(),
                            self.read_limit,
                        )?;
                        HostResponse::Error(aside_vm::NewFault {
                            value,
                            code: aside_vm::FaultCode::core(FaultKind::Type),
                            cause: None,
                        })
                    }
                    Err(error) => return Err(error),
                };
                self.vm.respond(ticket, response)?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::rich_escape" => {
                let input = self.vm.host_arguments(ticket)?[0];
                let source = text::read_string(self.vm.heap(), input, self.read_limit)?;
                let value = text::alloc_string(
                    self.vm.heap_mut(),
                    &source.replace('[', "[["),
                    self.read_limit,
                )?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::tr"
            | "reel::localization::load"
            | "reel::localization::locale"
            | "reel::localization::set_locale" => {
                if symbol != "reel::localization::locale" && symbol != "reel::tr" {
                    self.check_wait(session, ticket)?;
                }
                self.dispatch_localization(ticket, symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::video::play"
            | "reel::video::seek"
            | "reel::video::position"
            | "reel::audio::play"
            | "reel::audio::queue"
            | "reel::audio::volume"
            | "reel::audio::await"
            | "reel::audio::group_volume"
            | "reel::audio::pan"
            | "reel::audio::rate"
            | "reel::audio::seek"
            | "reel::audio::at"
            | "reel::video::at"
            | "reel::audio::position"
            | "reel::audio::stop" => {
                if symbol == "reel::audio::await" {
                    self.check_wait(session, ticket)?;
                }
                self.dispatch_media(session, ticket, symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::game::dialogue"
            | "reel::game::choice"
            | "reel::game::history"
            | "reel::reading::clear_history"
            | "reel::game::reading"
            | "reel::game::clear_read_progress"
            | "reel::reading::set_auto"
            | "reel::reading::set_skip"
            | "reel::reading::pause"
            | "reel::dialogue::clear"
            | "reel::choice::select" => {
                self.dispatch_narrative(session, ticket, symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::view" => {
                self.dispatch_view(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::when" => {
                self.dispatch_when(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::when::evaluate" => {
                self.evaluate_condition(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::completion"
            | "reel::stage::presented"
            | "reel::spawn"
            | "reel::scope::spawn"
            | "reel::task::cancel"
            | "reel::completion::resolve"
            | "reel::completion::reject"
            | "reel::completion::cancel"
            | "reel::completion::await" => {
                if symbol == "reel::completion::await" {
                    self.check_wait(session, ticket)?;
                }
                self.dispatch_result(session, ticket, symbol)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::list::scroll_to" => {
                self.dispatch_list_scroll_to(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::stage::geometry" => {
                self.dispatch_geometry(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::scroll::reveal_child" => {
                self.dispatch_scroll_reveal(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::list" => self.dispatch_list(ticket, budget),
            "reel::layered_image" => {
                let work = self.dispatch_layered_image(ticket)?;
                Ok(NativeDispatch::Ready { work })
            }
            "reel::derive" => {
                self.vm.respond_derive(ticket)?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::model" => {
                let work = self.vm.respond_model(ticket, work)?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work })
            }
            "reel::game::configure" => {
                self.configure_game(ticket)?;
                Ok(NativeDispatch::Ready { work: 4 })
            }
            "reel::sequence"
            | "reel::parallel"
            | "reel::action::then"
            | "reel::action::together" => {
                let pair = symbol.starts_with("reel::action::");
                let kind = if matches!(symbol, "reel::sequence" | "reel::action::then") {
                    activity::action::Kind::Sequence
                } else {
                    activity::action::Kind::Parallel
                };
                let work = activity::action::compose(
                    &mut self.vm,
                    &self.stage.types,
                    ticket,
                    kind,
                    pair,
                    work,
                )?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work })
            }
            "reel::scope" | "reel::reading" => {
                self.dispatch_scope(session, ticket, symbol == "reel::reading")?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::scope::cancel" => {
                self.dispatch_scope_exit(ticket, scopes::ScopeExit::Cancel)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::scope::finish" => {
                self.dispatch_scope_exit(ticket, scopes::ScopeExit::Finish)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::scope::wait" => {
                self.dispatch_scope_wait(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::scope::override" => {
                self.dispatch_scope_override(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::scope::offset" => {
                self.dispatch_scope_offset(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::override::remove" => {
                self.dispatch_override_remove(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::scope::add" => {
                self.dispatch_scope_add(ticket)?;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::run" | "reel::clock" | "reel::scope::run" => {
                let args = self.vm.host_arguments(ticket)?;
                let [action, owner, paused] = if symbol == "reel::scope::run" {
                    [args[1], args[0], args[2]]
                } else {
                    [args[0], args[1], args[2]]
                };
                let skip = (symbol != "reel::clock").then(|| (args[3], session.reading_id));
                let owner = if owner.tag() == aside_vm::ValueTag::Null {
                    self.default_activity_owner(ticket.task)?
                } else {
                    owner
                };
                let start = if symbol == "reel::clock" {
                    activity::Activities::clock
                } else {
                    activity::Activities::run
                };
                let (done, work) = start(
                    &mut self.activities,
                    &mut self.vm,
                    &self.stage,
                    self.root_owner,
                    ticket,
                    activity::Launch {
                        action,
                        owner,
                        paused,
                        skip,
                    },
                    work,
                )?;
                if done {
                    self.issued = None;
                    Ok(NativeDispatch::Ready { work })
                } else {
                    Ok(NativeDispatch::Decoding { work })
                }
            }
            "reel::tween" | "reel::repeat" | "reel::delay" => {
                let args = self.vm.host_arguments(ticket)?.to_vec();
                let value = match symbol {
                    "reel::delay" => activity::action::delay(self.vm.heap_mut(), args[0])?,
                    "reel::tween" => activity::action::tween(
                        self.vm.heap_mut(),
                        &self.stage,
                        &args,
                        self.read_limit,
                    )?,
                    "reel::repeat" => {
                        activity::action::repeat(self.vm.heap_mut(), args[0], args[1])?
                    }
                    _ => unreachable!("registered activity constructor"),
                };
                self.stage
                    .types
                    .mark(self.vm.heap_mut(), value, "reel::types::Action")?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::emit" => self.dispatch_emit(ticket, budget),
            "reel::clock::next" => {
                self.check_wait(session, ticket)?;
                self.activities.next_tick(&mut self.vm, ticket)?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::pause"
            | "reel::resume"
            | "reel::stop"
            | "reel::finish"
            | "reel::activity::await" => {
                if results::kind(self.vm.heap(), self.vm.host_arguments(ticket)?[0]).ok()
                    == Some(results::Kind::Particle)
                {
                    self.control_particles(ticket, symbol)?;
                    return Ok(NativeDispatch::Ready { work: 1 });
                }
                if media::is_media(self.vm.heap(), self.vm.host_arguments(ticket)?[0]) {
                    self.dispatch_media(session, ticket, symbol)?;
                    return Ok(NativeDispatch::Ready { work: 1 });
                }
                let command = match symbol {
                    "reel::pause" => activity::Command::Pause,
                    "reel::resume" => activity::Command::Resume,
                    "reel::stop" => activity::Command::Stop,
                    "reel::finish" => activity::Command::Finish,
                    _ => {
                        self.check_wait(session, ticket)?;
                        activity::Command::Await
                    }
                };
                let (done, work) = self.activities.command(
                    &mut self.vm,
                    &self.stage,
                    self.root_owner,
                    ticket,
                    command,
                    work,
                )?;
                if done {
                    self.issued = None;
                    Ok(NativeDispatch::Ready { work })
                } else {
                    Ok(NativeDispatch::Decoding { work })
                }
            }
            "reel::character" | "reel::character::line" => {
                if symbol == "reel::character" {
                    self.character(ticket)?;
                } else {
                    self.character_line(ticket)?;
                }
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::character::speak" => self.character_speak(session, ticket, locale, budget),
            "reel::speech::append" | "reel::speech::wait" | "reel::speech::revealed" => {
                let command = match symbol {
                    "reel::speech::append" => dialogue::SpeechCommand::Append,
                    "reel::speech::wait" => dialogue::SpeechCommand::Wait,
                    _ => dialogue::SpeechCommand::Revealed,
                };
                self.dispatch_speech(session, ticket, command, budget)
            }
            "reel::unbind" => {
                let reference = self.vm.host_arguments(ticket)?[0];
                let value = self.vm.heap_mut().unbind_property(reference)?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::say" => self.dispatch_say(session, ticket, locale, budget),
            "reel::game::record" => self.record_history(session, ticket, locale, budget),
            "reel::narrator" => self.dispatch_narrator(session, ticket, locale, budget),
            stage::add::SYMBOL
            | stage::remove::SYMBOL
            | stage::clear::SYMBOL
            | stage::move_child::SYMBOL => {
                let invoke = match symbol {
                    stage::add::SYMBOL => stage::add::invoke,
                    stage::remove::SYMBOL => stage::remove::invoke,
                    stage::move_child::SYMBOL => stage::move_child::invoke,
                    _ => stage::clear::invoke,
                };
                invoke(&mut self.stage, &mut self.vm, ticket)?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            stage::input::binding::SYMBOL | stage::input::value::SYMBOL => {
                let invoke = if symbol == stage::input::binding::SYMBOL {
                    stage::input::binding::invoke
                } else {
                    stage::input::value::invoke
                };
                invoke(&mut self.stage, &mut self.vm, ticket)?;
                self.issued = None;
                Ok(NativeDispatch::Ready { work: 1 })
            }
            "reel::choose" => self.dispatch_choose(session, ticket, work),
            "reel::text_input::select_bytes"
            | "reel::text_input::select_all"
            | "reel::text_input::undo"
            | "reel::text_input::redo"
            | stage::text_input::filter::SYMBOL => {
                let name = symbol.rsplit("::").next().unwrap();
                let (progress, change) = stage::text_input::dispatch(
                    &mut self.stage,
                    &mut self.vm,
                    ticket,
                    name,
                    budget,
                )?;
                self.issued = None;
                if let Some((node, target, closure, value)) = change {
                    self.start_text_callback(session, node, target, closure, value)?;
                }
                Ok(progress)
            }
            "reel::text"
            | "reel::text_input"
            | "reel::text_area"
            | "reel::slider"
            | "reel::progress_bar"
            | "reel::toggle"
            | "reel::checkbox"
            | "reel::radio"
            | "reel::button"
            | "reel::input"
            | "reel::image"
            | "reel::video"
            | "reel::particles"
            | "reel::row"
            | "reel::column"
            | "reel::grid"
            | "reel::camera"
            | "reel::camera_plane"
            | "reel::scroll"
            | "reel::popup"
            | "reel::tooltip"
            | "reel::stack"
            | "reel::cursor"
            | "reel::stage::replace" => {
                let arguments = self.vm.host_arguments(ticket)?.to_vec();
                let origin = self.stage_origin(ticket);
                let value = self.stage.dispatch(
                    &mut self.vm,
                    self.root_owner,
                    symbol,
                    &arguments,
                    self.read_limit,
                    origin,
                )?;
                self.vm.respond(ticket, HostResponse::Ready(value))?;
                self.issued = None;
                Ok(NativeDispatch::Ready {
                    work: arguments.len() as u32,
                })
            }
            _ => {
                let schema = stage::configure::schema(symbol)
                    .ok_or(AdapterError::Input("unregistered session native"))?;
                let progress = stage::configure::runtime::dispatch(
                    &self.stage,
                    &mut self.vm,
                    ticket,
                    schema,
                    budget,
                )?;
                self.issued = None;
                Ok(match progress {
                    stage::configure::runtime::Progress::Complete { work } => {
                        NativeDispatch::Ready { work }
                    }
                    stage::configure::runtime::Progress::Continue {
                        work,
                        required_work,
                    } => NativeDispatch::BudgetYield {
                        work,
                        required_work,
                    },
                })
            }
        }
    }

    pub(super) fn stage_origin(&self, ticket: HostTicket) -> Option<String> {
        let p = self.vm.program().package();
        ticket.location.source.map(|span| {
            let source = &p.sources[span.file.index()];
            let before = &source.text[..span.start as usize];
            let line = before.bytes().filter(|b| *b == b'\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap().chars().count() + 1;
            format!("{}:{line}:{column}", p.symbols[source.path.index()])
        })
    }

    pub(super) fn dispatch_choose(
        &mut self,
        session: &mut Session,
        ticket: HostTicket,
        budget: u32,
    ) -> Result<NativeDispatch, AdapterError> {
        self.check_story_wait(session, ticket)?;
        let mut reader = if let Some(reader) = self.choice_reader.take() {
            reader
        } else {
            let args = self.vm.host_arguments(ticket)?;
            let [options] = args else {
                return Err(AdapterError::Input("choose arity"));
            };
            let options = *options; // end the VM argument borrow before root insertion
            choice::ChoiceReader::new(self.vm.heap_mut(), options, self.root_owner)?
        };
        match reader.step(self.vm.heap_mut(), budget) {
            Ok((false, work)) => {
                self.choice_reader = Some(reader);
                Ok(NativeDispatch::Decoding { work })
            }
            Ok((true, work)) => {
                let result = self.publish_choice(session, ticket, &reader.rows);
                reader.release(self.vm.heap_mut())?;
                result.map(|interaction| NativeDispatch::Published { interaction, work })
            }
            Err(error) => {
                reader.release(self.vm.heap_mut())?;
                Err(error)
            }
        }
    }
}
