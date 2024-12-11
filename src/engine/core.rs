use crate::engine::api::OrderBookEntry;
use crate::engine::engine_metrics::EngineMetrics;
use crate::engine::models::{Order, PriceUpdate, Trade, TradingPair};
use crate::engine::order_book::{OrderBook, SimpleOrderBook};
use std::collections::HashMap;
use std::sync::Once;
use tokio::sync::mpsc;
use tracing::info;

static INIT: Once = Once::new();

pub enum Message {
    NewOrder(Order),
    PriceUpdate(PriceUpdate),
    MatchOrders(TradingPair),
    GetPrice(TradingPair, mpsc::Sender<Option<f64>>),
    GetOrderBook(
        TradingPair,
        mpsc::Sender<(Vec<OrderBookEntry>, Vec<OrderBookEntry>)>,
    ),
    GetTradeHistory(TradingPair, mpsc::Sender<Vec<Trade>>),
    Shutdown,
}

// TODO: Implement features and remove dead code
#[allow(dead_code)]
pub struct Engine {
    order_books: HashMap<TradingPair, Box<dyn OrderBook>>,
    metrics: EngineMetrics,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

impl Engine {
    pub fn new() -> Self {
        INIT.call_once(|| {
            tracing_subscriber::fmt()
                .with_target(false)
                .with_thread_ids(true)
                .with_level(true)
                .with_file(true)
                .with_line_number(true)
                .with_env_filter("info")
                .init();

            let builder = metrics_exporter_prometheus::PrometheusBuilder::new();
            builder
                .install()
                .expect("Failed to install Prometheus recorder.");
        });

        Engine {
            order_books: HashMap::new(),
            metrics: EngineMetrics::new(),
        }
    }

    async fn process_get_price(
        &mut self,
        trading_pair: TradingPair,
        response_tx: mpsc::Sender<Option<f64>>,
    ) {
        info!("Processing get_price request for {:?}", trading_pair);

        let price = if let Some(order_book) = self.order_books.get(&trading_pair) {
            info!("Found existing order book");
            let price = order_book.get_current_price().await;
            info!("Got price from existing order book: {:?}", price);
            price
        } else {
            info!("Creating new order book");
            let order_book = SimpleOrderBook::new(trading_pair.clone());
            let price = order_book.get_current_price().await;
            info!("Got price from new order book: {:?}", price);
            self.order_books.insert(trading_pair, Box::new(order_book));
            price
        };

        info!("Attempting to send price {:?} through channel", price);
        if let Err(e) = response_tx.send(price).await {
            eprintln!("Failed to send price response: {}", e);
        } else {
            info!("Successfully sent price through channel");
        }
    }

    async fn process_get_order_book(
        &mut self,
        trading_pair: TradingPair,
        response_tx: mpsc::Sender<(Vec<OrderBookEntry>, Vec<OrderBookEntry>)>,
    ) {
        if let Some(order_book) = self.order_books.get(&trading_pair) {
            let (bids, asks) = order_book.get_order_book().await;
            let _ = response_tx.send((bids, asks)).await;
        } else {
            let _ = response_tx.send((vec![], vec![])).await;
        }
    }

    async fn process_get_trade_history(
        &mut self,
        trading_pair: TradingPair,
        response_tx: mpsc::Sender<Vec<Trade>>,
    ) {
        if let Some(order_book) = self.order_books.get(&trading_pair) {
            let trades = order_book.get_trade_history().await;
            let _ = response_tx.send(trades).await;
        } else {
            let _ = response_tx.send(vec![]).await;
        }
    }

    async fn shutdown(&mut self) {
        info!("Initiating engine shutdown...");

        // Complete any pending matches
        for (trading_pair, order_book) in &self.order_books {
            info!("Processing final matches for {:?}", trading_pair);
            let trades = order_book.match_orders().await;
            if !trades.is_empty() {
                info!("Executed {} final trades", trades.len());
            }
        }

        // Log final metrics
        let total_orders: usize = self
            .order_books
            .values()
            .map(|book| futures::executor::block_on(book.get_active_orders_count()))
            .sum();

        info!(
            "Engine shutdown complete. Final state: {} active orders",
            total_orders
        );
    }

    pub async fn run(&mut self, mut rx: mpsc::Receiver<Message>) {
        info!("Starting engine");
        while let Some(message) = rx.recv().await {
            match message {
                Message::NewOrder(order) => {
                    let order_book = self
                        .order_books
                        .entry(order.trading_pair.clone())
                        .or_insert_with(|| {
                            Box::new(SimpleOrderBook::new(order.trading_pair.clone()))
                        });
                    order_book.add_order(order).await;
                }
                Message::GetOrderBook(trading_pair, response_tx) => {
                    self.process_get_order_book(trading_pair, response_tx).await;
                }
                Message::GetTradeHistory(trading_pair, response_tx) => {
                    self.process_get_trade_history(trading_pair, response_tx)
                        .await;
                }
                Message::PriceUpdate(update) => {
                    println!("Price update: {:?}", update);
                }
                Message::MatchOrders(trading_pair) => {
                    if let Some(order_book) = self.order_books.get(&trading_pair) {
                        let trades = order_book.match_orders().await;
                        println!("Executed trades for {:?}: {:?}", trading_pair, trades);
                    }
                }
                Message::GetPrice(trading_pair, response_tx) => {
                    self.process_get_price(trading_pair, response_tx).await;
                }
                Message::Shutdown => {
                    info!("Received shutdown signal");
                    self.shutdown().await;
                    break;
                }
            }
        }
        info!("Engine stopped");
    }
}

pub fn start_engine() -> mpsc::Sender<Message> {
    let (tx, rx) = mpsc::channel(100);

    tokio::spawn(async move {
        let mut engine = Engine::new();
        engine.run(rx).await;
    });

    tx
}