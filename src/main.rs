use actix::{Actor, Addr, AsyncContext, Handler, StreamHandler};
use actix_web::{get, web, App, HttpServer, HttpResponse, Responder};
use actix_web_actors::ws;
use actix::ActorContext;
use actix_web::post;
use serde::{Serialize, Deserialize};
use std::sync::{Arc, Mutex};
use uuid::{uuid, Uuid};
use std::fs::read_to_string;
use chrono::{DateTime, Utc}; // Import DateTime and Utc
use std::collections::HashMap;

// AppState to share among actors
pub struct AppState {
    chat_rooms: Arc<Mutex<HashMap<Uuid, ChatRoom>>>,
    main_room_id: Uuid, // Add this field
}

impl AppState{
    pub fn add_ws(&self, room_ids: Vec<Uuid>, user_addr: Addr<WsActor>) {
        let chat_rooms = match self.chat_rooms.lock() {
            Ok(rooms) => rooms,
            Err(e) => {
                eprintln!("Failed to lock chat_rooms: {:?}", e);
                return;
            }
        };

        for room_id in room_ids {
            if let Some(chat_room) = chat_rooms.get(&room_id) {
                let mut connections = match chat_room.connections.lock() {
                    Ok(conn) => conn,
                    Err(e) => {
                        eprintln!("Failed to lock connections for room {}: {:?}", room_id, e);
                        continue;
                    }
                };
                connections.push(user_addr.clone());
            } else {
                eprintln!("Room ID not found: {:?}", room_id);
            }
        }
    }

    pub fn remove_ws(&self, room_ids: Vec<Uuid>, user_addr: Addr<WsActor>) {
        let chat_rooms = match self.chat_rooms.lock() {
            Ok(rooms) => rooms,
            Err(e) => {
                eprintln!("Failed to lock chat_rooms: {:?}", e);
                return;
            }
        };

        for room_id in room_ids {
            if let Some(chat_room) = chat_rooms.get(&room_id) {
                let mut connections = match chat_room.connections.lock() {
                    Ok(conn) => conn,
                    Err(e) => {
                        eprintln!("Failed to lock connections for room {}: {:?}", room_id, e);
                        continue;
                    }
                };

                connections.retain(|addr| *addr != user_addr);
            } else {
                eprintln!("Room ID not found: {:?}", room_id);
            }
        }
    }

    pub fn catch_up(&self, room_id: Uuid) -> Vec<Message> {
        let chat_rooms = self.chat_rooms.lock().unwrap();
        match chat_rooms.get(&room_id) {
            Some(chat_room) => {
                chat_room.messages.lock().unwrap().clone()
            },
            None => Vec::new(),
        }
    }
}

struct ChatRoom {
    name: String,
    messages: Arc<Mutex<Vec<Message>>>,
    connections: Arc<Mutex<Vec<Addr<WsActor>>>>,
}

impl ChatRoom {
    pub fn new(name: String) -> Self {
        Self {
            name,
            messages: Arc::new(Mutex::new(Vec::new())),
            connections: Arc::new(Mutex::new(Vec::new())),
        }
    }

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
    room_id: Uuid,
    username: String,
}

struct WsMessage(pub String, pub Uuid);

impl actix::Message for WsMessage {
    type Result = ();
}

// Define HTTP actor for WebSocket
struct WsActor {
    id: Uuid,
    username: String,
    current_room: Uuid,
    rooms: Vec<Uuid>, // List of room IDs the actor is part of
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
        // Get the actor's address
        let actor_addr = ctx.address();

        // Call the remove_ws function on the AppState
        // Here, we clone the rooms vector and the AppState Arc to pass them to the function
        let rooms_to_join = self.rooms.clone();
        let app_state = self.state.clone();

        // Since we're in an asynchronous context, we need to spawn a blocking task to handle this
        // This is important to avoid blocking the async runtime with the mutex lock inside remove_ws
        actix::spawn(async move {
            app_state.add_ws(rooms_to_join, actor_addr);
        });

        // Catch up on messages
        let app_state = self.state.clone();
        let rooms = self.rooms.clone();
        for room_id in rooms {
            let messages = app_state.catch_up(room_id);
            for message in messages {
                let serialized_msg = serde_json::to_string(&message).unwrap();
                ctx.text(serialized_msg); // Send each message to the actor
            }
        }
    }

    fn stopped(&mut self, ctx: &mut Self::Context) {
        // Get the actor's address
        let actor_addr = ctx.address();

        // Call the remove_ws function on the AppState
        // Here, we clone the rooms vector and the AppState Arc to pass them to the function
        let rooms_to_leave = self.rooms.clone();
        let app_state = self.state.clone();

        // Since we're in an asynchronous context, we need to spawn a blocking task to handle this
        // This is important to avoid blocking the async runtime with the mutex lock inside remove_ws
        actix::spawn(async move {
            app_state.remove_ws(rooms_to_leave, actor_addr);
        });
    }
}

impl Handler<WsMessage> for WsActor {
    type Result = ();

    fn handle(&mut self, msg: WsMessage, ctx: &mut Self::Context) {
        // Always send the message to the client, including the sender
        ctx.text(msg.0);
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WsActor {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        if let Ok(ws::Message::Text(text)) = msg {
            // Handle changing rooms
            if text.starts_with("add_room:") {
                let replaced_text = text.replace("add_room:", "");
                let room_id_str = replaced_text.trim();
                if let Ok(room_id) = Uuid::parse_str(room_id_str) {
                    let app_state = self.state.clone();
                    let mut chat_rooms = app_state.chat_rooms.lock().unwrap();


                    // Add actor to new room and update room in WsActor state
                    if let Some(chat_room) = chat_rooms.get_mut(&room_id) {
                        chat_room.connections.lock().unwrap().push(ctx.address());
                        self.rooms.push(room_id);

                        // Optionally send previous messages from the new room
                        let messages = chat_room.messages.lock().unwrap();
                        for message in messages.iter() {
                            let serialized_msg = serde_json::to_string(message).unwrap();
                            ctx.text(serialized_msg); // Send each message to the actor
                        }
                    } else {
                        eprintln!("Chat room not found with ID: {:?}", room_id);
                    }
                } else {
                    eprintln!("Invalid room ID format: {:?}", room_id_str);
                }
            } else if text.starts_with("set_username:") {
                let new_username = text.replace("set_username:", "");
                self.set_username(new_username);
            } else {
                // Normal message handling
                match serde_json::from_str::<Message>(&text) {
                    Ok(mut message) => {
                        message.id = Some(Utc::now());
                        message.username = self.username.clone();
                        message.room_id = self.current_room;

                        // Broadcast the message to the current room
                        let app_state = self.state.clone();
                        let chat_rooms = app_state.chat_rooms.lock().unwrap();
                        if let Some(room) = chat_rooms.get(&self.current_room) {
                            //adds message to chatroom
                            room.messages.lock().unwrap().push(message.clone());
                            let serialized_msg = serde_json::to_string(&message).unwrap();
                            //broadcasts message
                            room.broadcast_message(serialized_msg, self.id);
                        }
                    },
                    Err(e) => eprintln!("Error processing message: {:?}", e),
                }
            }
        }
    }
}

async fn ws_index(req: actix_web::HttpRequest, stream: web::Payload, state: web::Data<AppState>) -> Result<HttpResponse, actix_web::Error> {
    let main_room_id = state.main_room_id;
    let current_rooms = vec![state.main_room_id];
    ws::start(WsActor { 
        id: Uuid::new_v4(), 
        username: "None".to_string(), 
        current_room: main_room_id,
        rooms: current_rooms,
        state: state.into_inner().clone(), // Updated line
    }, &req, stream)
}


#[get("/login")]
async fn login_form() -> impl Responder {
    let path = "static/login_page.html";
    match read_to_string(path) {
        Ok(content) => HttpResponse::Ok().content_type("text/html").body(content),
        Err(err) => {
            eprintln!("Failed to read homepage HTML: {:?}", err);
            HttpResponse::InternalServerError().finish()
        }
    }
}

/*Login TODO
#[post("/login")]
async fn login(req: HttpRequest, form: web::Form<LoginForm>, session: Session) -> impl Responder {
    // Logic to check user credentials
    if authenticate_user(&form.username, &form.password) {
        session.insert("user", form.username.clone()).unwrap();
        HttpResponse::Found().header("LOCATION", "/").finish()
    } else {
        HttpResponse::Unauthorized().body("Invalid credentials")
    }
}

#[get("/")]
async fn index(session: Session) -> impl Responder {
    if let Some(username) = session.get::<String>("user").unwrap() {
        HttpResponse::Ok().body(format!("Welcome, {}!", username))
    } else {
        HttpResponse::Found().header("LOCATION", "/login").finish()
    }
}
*/

#[get("/")]
async fn home_page() -> impl Responder {
    let path = "static/home_page.html";
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
    let chat_rooms = Arc::new(Mutex::new(HashMap::new()));
    let main_room_id = Uuid::new_v4(); // Create a UUID for the main room
    chat_rooms.lock().unwrap().insert(main_room_id, ChatRoom::new("Main Room".to_string()));

    let app_state = web::Data::new(AppState { 
        chat_rooms,
        main_room_id, // Initialize here
    });

    HttpServer::new(move || {
        App::new()
            .app_data(app_state.clone()) // Use app_state directly
            .service(home_page)
            .route("/ws/", web::get().to(ws_index))
            .service(actix_files::Files::new("/static", "static").show_files_listing())
    })
    .bind(("192.168.0.155", 8080))?
    .run()
    .await
}

