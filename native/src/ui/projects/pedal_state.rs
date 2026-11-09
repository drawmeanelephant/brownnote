// Included inside ui::pedals by the integrator, preserving its private ownership.
impl PadPedals {
    pub(super) fn project_chain_is_current(&self) -> bool {
        self.pending.is_none()
            && !matches!(self.status, Status::Rendering | Status::Error(_))
            && match &self.landed {
                Some((chain, _)) => chain == &self.chain,
                None => self.chain.stages.is_empty(),
            }
    }

    pub(super) fn restore_project_chain(&mut self, chain: Chain, retained_seconds: Option<f32>) {
        self.chain = chain;
        self.requested = retained_seconds.map(|_| self.chain.clone());
        self.landed = retained_seconds.map(|_| (self.chain.clone(), false));
        self.status = match retained_seconds {
            Some(seconds) => Status::Ready {
                seconds,
                clipped: false,
            },
            None => Status::Base,
        };
    }
}
