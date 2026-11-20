use crate::{GIVE_UP, RENEW, Token};

/// Cooperative holder model. Call before each work/result boundary, especially on wake.
/// Once stopped, this instance cannot be revived, even by a late acknowledgement.
pub struct Holder {
    pub token: Token,
    last_send: i64,
    stopped: bool,
}
impl Holder {
    pub fn new(token: Token, claim_sent: i64) -> Self {
        Self {
            token,
            last_send: claim_sent,
            stopped: false,
        }
    }
    pub fn may_work(&mut self, now: i64) -> bool {
        // Regressing elapsed time is a broken clock domain: fail closed.
        if now < self.last_send || now.saturating_sub(self.last_send) >= GIVE_UP {
            self.stopped = true;
        }
        !self.stopped
    }
    pub fn renewal_due(&mut self, now: i64) -> bool {
        self.may_work(now) && now - self.last_send >= RENEW
    }
    pub fn acknowledge(&mut self, sent: i64, received: i64) -> bool {
        if !self.may_work(received)
            || sent < self.last_send
            || sent > received
            || received.saturating_sub(sent) >= GIVE_UP
        {
            return false;
        }
        // Sending bounds the earliest time the daemon could have extended authority.
        self.last_send = sent;
        true
    }
}
