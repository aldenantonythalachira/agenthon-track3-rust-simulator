/// A single resting order at a price level.
#[derive(Clone, Copy, Debug)]
pub struct RestingOrder {
    pub order_id: i64,
    pub agent_id: i32,
    pub side: Side,
    pub price: i64,
    pub quantity: i64,
    pub t_ns: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side { Bid, Ask }

/// FIFO queue at a single price level.
#[derive(Default)]
pub struct PriceLevel {
    pub orders: Vec<RestingOrder>,
    pub total_qty: i64,
}

impl PriceLevel {
    pub fn push(&mut self, o: RestingOrder) {
        self.total_qty += o.quantity;
        self.orders.push(o);
    }

    pub fn peek(&self) -> Option<&RestingOrder> {
        self.orders.first()
    }

    pub fn consume_head(&mut self, qty: i64) -> Option<(RestingOrder, i64)> {
        let head = self.orders.first_mut()?;
        let take = qty.min(head.quantity);
        let mut filled = *head;
        filled.quantity = take;
        head.quantity -= take;
        self.total_qty -= take;
        if head.quantity == 0 {
            self.orders.remove(0);
        }
        Some((filled, take))
    }

    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }
}

/// The order book: side -> price -> FIFO queue.
/// Uses BTreeMap: deterministic iteration order, O(log P) access.
pub struct OrderBook {
    pub bids: std::collections::BTreeMap<i64, PriceLevel>,
    pub asks: std::collections::BTreeMap<i64, PriceLevel>,
    pub last_trade: Option<i64>,
}

impl OrderBook {
    pub fn new() -> Self {
        Self {
            bids: std::collections::BTreeMap::new(),
            asks: std::collections::BTreeMap::new(),
            last_trade: None,
        }
    }

    pub fn best_bid(&self) -> Option<i64> {
        self.bids.keys().next_back().copied()
    }

    pub fn best_ask(&self) -> Option<i64> {
        self.asks.keys().next().copied()
    }
        pub fn best_bid_qty(&self) -> i64 {
        self.bids.iter().next_back().map(|(_, l)| l.total_qty).unwrap_or(0)
    }

    pub fn best_ask_qty(&self) -> i64 {
        self.asks.iter().next().map(|(_, l)| l.total_qty).unwrap_or(0)
    }

    /// Try to match `order`. Returns list of (resting_order, fill_qty, fill_price).
    /// If order remains unfilled, rest it in the book.
    pub fn handle_limit(
        &mut self,
        mut order: RestingOrder,
    ) -> Vec<(RestingOrder, i64, i64)> {
        let mut fills = Vec::new();

        match order.side {
            Side::Bid => {
                while order.quantity > 0 {
                    let Some((&best_ask, _)) = self.asks.iter_mut().next() else { break };
                    if order.price < best_ask { break; }
                    let level = self.asks.get_mut(&best_ask).unwrap();
                    let Some((resting, take)) = level.consume_head(order.quantity) else { break };
                    fills.push((resting, take, best_ask));
                    order.quantity -= take;
                    self.last_trade = Some(best_ask);
                    if level.is_empty() {
                        self.asks.remove(&best_ask);
                    }
                }
                if order.quantity > 0 {
                    self.bids.entry(order.price).or_default().push(order);
                }
            }
            Side::Ask => {
                while order.quantity > 0 {
                    let Some((&best_bid, _)) = self.bids.iter_mut().next_back() else { break };
                    if order.price > best_bid { break; }
                    let level = self.bids.get_mut(&best_bid).unwrap();
                    let Some((resting, take)) = level.consume_head(order.quantity) else { break };
                    fills.push((resting, take, best_bid));
                    order.quantity -= take;
                    self.last_trade = Some(best_bid);
                    if level.is_empty() {
                        self.bids.remove(&best_bid);
                    }
                }
                if order.quantity > 0 {
                    self.asks.entry(order.price).or_default().push(order);
                }
            }
        }

        fills
    }
}
