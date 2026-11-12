// The "VIP only" quick filter, shared by the Mac list (MessageList) and the phone list (PhoneList). Session-only, like the other chips.
class VipFilter { on = $state(false); }
export const vipFilter = new VipFilter();
