//! Worker-side G1 pipeline; no recognition/rule matching occurs in native hooks.
//! No shipped binary starts this actor. Construction and rules are not consent.
#[path="gesture_worker_core.rs"]
mod core;

#[cfg(windows)]
mod native {
    use super::core::{Begin,Core,Finish};
    use crate::{context::{actions::{implemented_actions,LocalActions,PreparedLocalAction},watch::WatchError,ContextError},gesture_capture::{self,Event,GestureCapture,GestureReceiver,RecoveryOutcome}};
    use peerglide_actions::{rules::{RuleBook,RuleError},execution::{NativeFailure,Outcome,RequestId},Capabilities,Risk};
    use peerglide_gestures::Recognizer;
    use std::time::{Duration,Instant};

    #[derive(Clone,Copy,Debug,PartialEq,Eq)]
    pub enum WorkerState {Dormant,Running,Suspended,GesturePending,RecoveryPending,Stopping,Stopped,Fault}
    #[derive(Clone,Copy,Debug,PartialEq,Eq)]
    pub enum WorkerError {InvalidState,Watch(WatchError),CaptureUnavailable,CleanupPending,OwnershipLost,Entropy,Action(NativeFailure)}
    #[derive(Clone,Copy,Debug,PartialEq,Eq)]
    pub enum Dispatch {Action(Outcome),RightClick(RecoveryOutcome),Cancelled}
    #[derive(Clone,Copy,Debug,PartialEq,Eq)]
    pub enum RuleUpdateError {Rules(RuleError),Worker(WorkerError)}
    struct Armed {app:String,dpi:u32,expires:Instant}

    /// One synchronous actor on its creating worker thread. LocalContexts makes
    /// this !Send; construct it inside the intended thread/process, never move a
    /// live native observer from the GUI. poll services bounded batches only.
    pub struct LocalGestureWorker<'a> {
        core:Core<'a,PreparedLocalAction>,
        actions:Option<LocalActions>,
        capture:Option<GestureCapture>,
        receiver:Option<GestureReceiver>,
        armed:Option<Armed>,
        capabilities:Capabilities,
        destructive:bool,
        maximum:Duration,
        state:WorkerState,
        last:Option<Dispatch>,
        error:Option<WorkerError>,
    }
    impl<'a> LocalGestureWorker<'a> {
        /// Pure rule setup. No identity, peer, native capture or OS effect.
        pub fn new(recognizer:&'a Recognizer,book:RuleBook)->Result<Self,RuleError>{
            Ok(Self{core:Core::new(recognizer,book)?,actions:None,capture:None,receiver:None,armed:None,capabilities:implemented_actions(),destructive:false,maximum:recognizer.config().max_duration.min(gesture_capture::MAX_HOLD).min(peerglide_actions::MAX_ACTION_LIFETIME),state:WorkerState::Dormant,last:None,error:None})
        }
        pub fn state(&self)->WorkerState{self.state}
        pub fn last_dispatch(&self)->Option<Dispatch>{self.last}
        pub fn last_error(&self)->Option<WorkerError>{self.error}
        /// Trusted local UI/worker consent only, never a saved rule enabled flag
        /// or an input/data peer grant. No product binary calls this yet.
        pub fn start_after_explicit_consent(&mut self,destructive_confirmed:bool)->Result<(),WorkerError>{
            if self.state!=WorkerState::Dormant{return Err(WorkerError::InvalidState);}
            let setup=(||{let mut actions=LocalActions::new().map_err(WorkerError::Watch)?;actions.poll().map_err(WorkerError::Watch)?;actions.consent_control().explicitly_allow(destructive_confirmed).map_err(WorkerError::Watch)?;Ok(actions)})();
            let actions=match setup{Ok(actions)=>actions,Err(error)=>{self.state=WorkerState::Fault;self.error=Some(error);return Err(error);}};
            match gesture_capture::capture(actions.trigger_source()){
                Ok((capture,receiver))=>{self.actions=Some(actions);self.capture=Some(capture);self.receiver=Some(receiver);self.destructive=destructive_confirmed;self.state=WorkerState::Running;self.error=None;Ok(())}
                Err(_)=>{let _=actions.shutdown();self.state=WorkerState::Fault;self.error=Some(WorkerError::CaptureUnavailable);Err(WorkerError::CaptureUnavailable)}
            }
        }
        fn cancel_prepared(&mut self,item:(u64,PreparedLocalAction)){
            if let Some(actions)=self.actions.as_mut(){let _=actions.cancel(item.1);actions.trigger_source().invalidate();}
            if let Some(capture)=self.capture.as_mut(){capture.control().retire(item.0);}
            self.armed=None;self.last=Some(Dispatch::Cancelled);
        }
        fn cancel_pending(&mut self){if let Some(item)=self.core.cancel(){self.cancel_prepared(item);}}
        /// Validate before adopting. Invalid/future rule data leaves the active
        /// snapshot intact and is never written back to a user file here.
        pub fn replace_rules(&mut self,book:RuleBook)->Result<(),RuleUpdateError>{
            if let Some(item)=self.core.replace_rules(book).map_err(RuleUpdateError::Rules)?{self.cancel_prepared(item);}
            self.armed=None;
            if let Some(actions)=self.actions.as_mut(){if let Err(error)=actions.configuration_changed(){self.error=Some(WorkerError::Watch(error));let _=self.stop();return Err(RuleUpdateError::Worker(WorkerError::Watch(error)));}}
            Ok(())
        }
        fn retire(&mut self,sequence:u64){
            if let Some(capture)=self.capture.as_mut(){capture.control().retire(sequence);}
            if let Some(actions)=self.actions.as_ref(){actions.trigger_source().invalidate();}
            self.armed=None;
        }
        fn dispatch(&mut self,finish:Finish<PreparedLocalAction,gesture_capture::RecoveryReceipt>){
            match finish {
                Finish::Cancelled{sequence,prepared}=>self.cancel_prepared((sequence,prepared)),
                Finish::Action{sequence,prepared,spec}=>{
                    let mut id=[0;16];
                    if getrandom::fill(&mut id).is_err(){self.error=Some(WorkerError::Entropy);self.cancel_prepared((sequence,prepared));return;}
                    let outcome=self.actions.as_mut().map(|actions|actions.run(prepared,RequestId(id),spec));
                    self.retire(sequence);
                    if let Some(outcome)=outcome{self.action_completed(outcome);}
                }
                Finish::Click{sequence,prepared,receipt}=>{
                    if let (Some(actions),Some(capture))=(self.actions.as_mut(),self.capture.as_mut()){
                        match actions.recover_click(prepared,receipt,capture.control()){
                            Ok(outcome)=>self.last=Some(Dispatch::RightClick(outcome)),
                            Err(error)=>{self.last=Some(Dispatch::Cancelled);self.error=Some(WorkerError::Action(error));}
                        }
                    }
                    self.retire(sequence);
                    if self.error==Some(WorkerError::Action(NativeFailure::PermissionLost)){self.native_error(NativeFailure::PermissionLost);}
                }
            }
        }
        fn handle(&mut self,event:gesture_capture::GestureEvent){
            match event {
                Event::Begin{sequence,trigger,point,at}=>{
                    let Some(frame)=self.armed.take() else{self.retire(sequence);return;};
                    if Instant::now()>=frame.expires{self.retire(sequence);return;}
                    let Some(actions)=self.actions.as_mut() else{self.retire(sequence);return;};
                    match actions.freeze_for_gesture(trigger,frame.dpi,self.maximum){
                        Ok(prepared)=>{
                            let input=Begin{sequence,context:prepared.context(),expires:prepared.expires(),prepared,app:frame.app,dpi:frame.dpi,point,at};
                            let cancelled=self.core.begin(input);let conflict=!cancelled.is_empty();
                            for item in cancelled{self.cancel_prepared(item);}
                            if conflict{self.cancel_pending();}
                        }
                        Err(error)=>{self.retire(sequence);self.native_error(error);}
                    }
                }
                Event::Move{sequence,point,at}=>{if let Some(item)=self.core.motion(sequence,point,at){self.cancel_prepared(item);}}
                Event::End{sequence,point,at,recovery}=>{
                    if let Some(finish)=self.core.end(sequence,point,at,recovery){self.dispatch(finish);}else{self.retire(sequence);}
                }
            }
        }
        fn action_completed(&mut self,outcome:Outcome){
            self.last=Some(Dispatch::Action(outcome));
            if matches!(outcome,Outcome::Failed(NativeFailure::PermissionLost)|Outcome::Rejected(peerglide_actions::Rejection::Disabled)){self.native_error(NativeFailure::PermissionLost);}
        }
        fn native_error(&mut self,error:NativeFailure){
            self.error=Some(WorkerError::Action(error));
            if error==NativeFailure::PermissionLost{let _=self.stop();}
        }
        fn admission_error(&mut self,error:NativeFailure)->Result<WorkerState,WorkerError>{
            if error==NativeFailure::PermissionLost{self.native_error(error);Err(WorkerError::Action(error))}else{Ok(self.suspend())}
        }
        fn transient(error:WatchError)->bool{
            matches!(error,WatchError::NotReady|WatchError::Stale|WatchError::Busy|WatchError::Backlog|WatchError::Context(ContextError::NoForeground|ContextError::NoFocus|ContextError::ChangedDuringRead|ContextError::IdentifierUnavailable|ContextError::IdentifierTooLong|ContextError::QueryFailed))
        }
        fn suspend(&mut self)->WorkerState{
            self.cancel_pending();self.armed=None;
            if let Some(actions)=self.actions.as_ref(){actions.trigger_source().invalidate();}
            if let Some(capture)=self.capture.as_mut(){let cancelled=capture.control().cancelled_through();capture.control().retire(cancelled);}
            self.state=WorkerState::Suspended;
            if let Some(capture)=self.capture.as_mut(){capture.control().heartbeat();if capture.control().recovery_pending(){self.state=WorkerState::RecoveryPending;}}
            self.state
        }
        /// Caller must poll promptly (well below the existing 1s watchdog), on
        /// this same native owner thread. No IPC payload contains raw input.
        pub fn poll(&mut self)->Result<WorkerState,WorkerError>{
            if self.state==WorkerState::Stopping{return self.stop().map(|_|self.state);}
            if matches!(self.state,WorkerState::Dormant|WorkerState::Stopped|WorkerState::Fault){return Ok(self.state);}
            let capture=self.capture.as_mut().ok_or(WorkerError::InvalidState)?;
            if capture.control().ownership_lost(){self.error=Some(WorkerError::OwnershipLost);let _=self.stop();return Err(WorkerError::OwnershipLost);}
            if self.receiver.as_ref().is_none_or(|receiver|!receiver.producer_alive()){self.error=Some(WorkerError::CaptureUnavailable);let _=self.stop();return Err(WorkerError::CaptureUnavailable);}
            let current=self.actions.as_mut().ok_or(WorkerError::InvalidState)?.poll();
            if let Err(error)=current{if Self::transient(error){return Ok(self.suspend());}self.error=Some(WorkerError::Watch(error));let _=self.stop();return Err(WorkerError::Watch(error));}
            // A new scope proof is read before processing any buffered actions.
            // Fullscreen/menu/capture/unknown scope pauses, never reuses old app data.
            if self.actions.as_ref().is_none_or(|actions|!actions.consent_is_live()){return self.admission_error(NativeFailure::PermissionLost);}
            let frame=match self.actions.as_mut().ok_or(WorkerError::InvalidState)?.capture_admission(){Ok(frame)=>frame,Err(error)=>return self.admission_error(error)};
            let stale=self.core.prepared().is_some_and(|prepared|self.actions.as_ref().is_none_or(|actions|!actions.prepared_is_current(prepared)));
            if stale{self.cancel_pending();}
            let cancelled=self.capture.as_mut().ok_or(WorkerError::InvalidState)?.control().cancelled_through();
            if let Some(item)=self.core.tick(Instant::now(),cancelled){self.cancel_prepared(item);}
            self.capture.as_mut().ok_or(WorkerError::InvalidState)?.control().retire(cancelled);
            self.capture.as_mut().ok_or(WorkerError::InvalidState)?.control().heartbeat();
            for _ in 0..gesture_capture::CAPACITY {
                let event=self.receiver.as_mut().ok_or(WorkerError::InvalidState)?.try_recv();
                let Some(event)=event else{break;};self.handle(event);
                if matches!(self.state,WorkerState::Stopping|WorkerState::Stopped|WorkerState::Fault){return Ok(self.state);}
            }
            let capture=self.capture.as_mut().ok_or(WorkerError::InvalidState)?;
            if capture.control().ownership_lost(){self.error=Some(WorkerError::OwnershipLost);let _=self.stop();return Err(WorkerError::OwnershipLost);}
            if self.receiver.as_ref().is_none_or(|receiver|!receiver.producer_alive()){self.error=Some(WorkerError::CaptureUnavailable);let _=self.stop();return Err(WorkerError::CaptureUnavailable);}
            if capture.control().recovery_pending(){self.state=WorkerState::RecoveryPending;return Ok(self.state);}
            if self.core.sequence().is_some(){self.state=WorkerState::GesturePending;return Ok(self.state);}
            self.state=WorkerState::Running;
            if !capture.control().has_right_debt(){
                let app=frame.application_id().to_owned();let dpi=frame.dpi();let expires=frame.expires();
                let destructive=self.destructive;let capabilities=&self.capabilities;
                if self.core.eligible(&app,|spec|capabilities.supports(spec.action)&&(destructive||spec.action.risk()!=Risk::Destructive)){
                    if capture.control().arm(frame).is_ok(){self.armed=Some(Armed{app,dpi,expires});}
                }else{self.armed=None;if let Some(actions)=self.actions.as_ref(){actions.trigger_source().invalidate();}}
            }
            Ok(self.state)
        }
        /// Revoke first; never call Stopped merely from a stop request. Failed
        /// recovery retains its native capture/reaper and reports pending/lost.
        pub fn stop(&mut self)->Result<(),WorkerError>{
            if self.state==WorkerState::Dormant||self.state==WorkerState::Stopped{self.state=WorkerState::Stopped;return Ok(());}
            self.state=WorkerState::Stopping;self.cancel_pending();self.armed=None;
            if let Some(capture)=self.capture.as_mut(){capture.control().stop();}
            let context_error=self.actions.as_mut().and_then(|actions|actions.shutdown().err());
            if let Some(capture)=self.capture.as_mut(){
                if capture.shutdown().is_err(){
                    let error=if capture.control().ownership_lost(){WorkerError::OwnershipLost}else{WorkerError::CleanupPending};
                    self.error=Some(error);return Err(error);
                }
            }
            if let Some(error)=context_error{self.error=Some(WorkerError::Watch(error));self.state=WorkerState::Fault;return Err(WorkerError::Watch(error));}
            self.capture=None;self.receiver=None;self.actions=None;self.state=WorkerState::Stopped;Ok(())
        }
    }
    impl Drop for LocalGestureWorker<'_>{fn drop(&mut self){let _=self.stop();}}
    #[cfg(test)]
    mod tests {
        use super::*;
        use peerglide_gestures::{Config,starter_templates};
        #[test]
        fn enabled_rule_preferences_do_not_construct_native_owners_or_grant_consent(){
            let recognizer=Recognizer::new(Config::default(),starter_templates()).unwrap();
            let mut worker=LocalGestureWorker::new(&recognizer,RuleBook{enabled:true,..RuleBook::default()}).unwrap();
            assert_eq!(worker.state(),WorkerState::Dormant);
            assert!(worker.actions.is_none()&&worker.capture.is_none()&&worker.receiver.is_none());
            assert!(!worker.destructive);assert!(worker.poll().is_ok());
            assert_eq!(worker.maximum,Duration::from_millis(2500));
            assert_eq!(worker.stop(),Ok(()));assert_eq!(worker.state(),WorkerState::Stopped);
            assert_eq!(worker.stop(),Ok(()));
            assert_eq!(worker.start_after_explicit_consent(true),Err(WorkerError::InvalidState));
            assert!(worker.actions.is_none()&&worker.capture.is_none());
        }
        #[test]
        fn actual_admission_failure_gate_stops_permission_loss_and_only_suspends_target_loss(){
            let recognizer=Recognizer::new(Config::default(),starter_templates()).unwrap();
            let mut worker=LocalGestureWorker::new(&recognizer,RuleBook::default()).unwrap();
            assert_eq!(worker.admission_error(NativeFailure::TargetLost),Ok(WorkerState::Suspended));
            assert_eq!(worker.admission_error(NativeFailure::PermissionLost),Err(WorkerError::Action(NativeFailure::PermissionLost)));
            assert_eq!(worker.state(),WorkerState::Stopped);
            assert!(worker.actions.is_none()&&worker.capture.is_none());
            let custom=Recognizer::new(Config{max_duration:Duration::from_secs(10),..Config::default()},starter_templates()).unwrap();
            let worker=LocalGestureWorker::new(&custom,RuleBook::default()).unwrap();
            assert_eq!(worker.maximum,gesture_capture::MAX_HOLD);
        }
        #[test]
        fn actual_action_outcome_gate_stops_revoked_consent_and_native_permission_loss(){
            let recognizer=Recognizer::new(Config::default(),starter_templates()).unwrap();
            for outcome in [Outcome::Failed(NativeFailure::PermissionLost),Outcome::Rejected(peerglide_actions::Rejection::Disabled)]{
                let mut worker=LocalGestureWorker::new(&recognizer,RuleBook::default()).unwrap();
                worker.action_completed(outcome);assert_eq!(worker.state(),WorkerState::Stopped);
                assert_eq!(worker.last_dispatch(),Some(Dispatch::Action(outcome)));
            }
        }
        #[test]
        fn invalid_rule_version_propagates_and_permission_loss_is_not_transient_focus(){
            let recognizer=Recognizer::new(Config::default(),starter_templates()).unwrap();
            let mut worker=LocalGestureWorker::new(&recognizer,RuleBook::default()).unwrap();
            assert_eq!(worker.replace_rules(RuleBook{schema_version:2,..RuleBook::default()}),Err(RuleUpdateError::Rules(RuleError::Schema)));
            assert_eq!(worker.state(),WorkerState::Dormant);
            assert!(LocalGestureWorker::transient(WatchError::NotReady));
            assert!(LocalGestureWorker::transient(WatchError::Context(ContextError::ChangedDuringRead)));
            assert!(!LocalGestureWorker::transient(WatchError::Context(ContextError::PermissionDenied)));
            assert!(!LocalGestureWorker::transient(WatchError::Closed));
            assert!(!LocalGestureWorker::transient(WatchError::CleanupFailed));
        }
    }

}
#[cfg(windows)]
pub use native::{Dispatch,LocalGestureWorker,RuleUpdateError,WorkerError,WorkerState};
