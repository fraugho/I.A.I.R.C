use actix::{Actor, Addr, AsyncContext, Handler, StreamHandler};
use actix_web::{get, web, App, HttpServer, HttpResponse, Responder};
use actix_web_actors::ws;
use serde::{Serialize, Deserialize};
use std::sync::{Arc, Mutex};
use uuid::Uuid;
use std::fs::read_to_string;
use chrono::{DateTime, Utc}; // Import DateTime and Utc

// AppState to share among actors
pub struct AppState {
    messages: Arc<Mutex<Vec<Message>>>,
    connections: Arc<Mutex<Vec<Addr<WsActor>>>>,
}

impl AppState {
    pub fn broadcast_message(&self, message: String, sender_id: Uuid) {
        let connections = self.connections.lock().unwrap();
        for client in connections.iter() {
            client.do_send(WsMessage(message.clone(), sender_id));
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Message {
    id: Option<DateTime<Utc>>,
    content: String,
    sender_id: Uuid,
    username: String, // Non-optional username
}

struct WsMessage(pub String, pub Uuid);

impl actix::Message for WsMessage {
    type Result = ();
}

// Define HTTP actor for WebSocket
struct WsActor {
    id: Uuid,
    username: String, // Non-optional username
    state: Arc<AppState>,
}

impl WsActor {
    fn set_username(&mut self, new_username: String) {
        self.username = new_username;
    }
}

impl Actor for WsActor {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, ctx: &mut Self::Context) {
        let addr = ctx.address();
        self.state.connections.lock().unwrap().push(addr);
    }

    fn stopped(&mut self, ctx: &mut Self::Context) {
        let mut connections = self.state.connections.lock().unwrap();
        connections.retain(|addr| addr != &ctx.address());
    }
}

impl Handler<WsMessage> for WsActor {
    type Result = ();

    fn handle(&mut self, msg: WsMessage, ctx: &mut Self::Context) {
        if self.id != msg.1 {
            ctx.text(msg.0);
        }
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WsActor {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        if let Ok(ws::Message::Text(text)) = msg {
            match serde_json::from_str::<Message>(&text) {
                Ok(mut message) => {
                    message.id = Some(Utc::now());
                    // Check if username is empty and set to "None" if it is
                    if message.username.is_empty() {
                        message.username = "None".to_string();
                    }
                    let serialized_msg = serde_json::to_string(&message).unwrap();
                    self.state.broadcast_message(serialized_msg, message.sender_id);
                },
                Err(e) => eprintln!("Error processing message: {:?}", e),
            }
        }
    }
}


async fn ws_index(req: actix_web::HttpRequest, stream: web::Payload, state: web::Data<Arc<AppState>>) -> Result<HttpResponse, actix_web::Error> {
    ws::start(WsActor { id: Uuid::new_v4(), username: "None".to_string(), state: state.get_ref().clone() }, &req, stream)
}

async fn send_message(state: web::Data<AppState>, message: web::Json<Message>) -> impl Responder {
    let msg = message.into_inner();

    let mut messages = state.messages.lock().unwrap();
    messages.push(msg.clone());

    let serialized_msg = serde_json::to_string(&msg).unwrap();

    state.broadcast_message(serialized_msg, msg.sender_id);

    HttpResponse::Ok().json("Message received")
}

#[get("/")]
async fn home_page() -> impl Responder {
    let path = "templates/home_page.html";
    match read_to_string(path) {
        Ok(content) => HttpResponse::Ok().content_type("text/html").body(content),
        Err(err) => {
            eprintln!("Failed to read homepage HTML: {:?}", err);
            HttpResponse::InternalServerError().finish()
        }
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let connections = Arc::new(Mutex::new(Vec::new()));
    let app_state = Arc::new(AppState { messages, connections });

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(app_state.clone()))
            .service(home_page)
            .route("/ws/", web::get().to(ws_index))
            .route("/send_message", web::post().to(send_message))
    })
    .bind(("192.168.0.155", 8080))?
    .run()
    .await
}
