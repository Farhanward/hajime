// Hajime WhatsApp bridge.
//
// Speaks WhatsApp's multi-device protocol through whatsmeow and exposes a
// small loopback HTTP API for hajime-wa to call. It is a separate process on
// purpose: the device keys live here, so restarting the Rust API never drops
// the session or forces a fresh QR scan.
//
//	GET  /session/{name}   -> {"name","status","phone"}
//	POST /send/text        <- {"session","chatId","text"}
//	                       -> {"id","chat_id"}
//
// Inbound messages are posted to HAJIME_WA_WEBHOOK in WAHA's event shape, so
// the existing n8n workflow keeps working unchanged.
//
// Environment:
//
//	HAJIME_WA_BRIDGE_BIND  listen address, default 127.0.0.1:3001
//	HAJIME_WA_STORE        session database, default ./hajime-wa.db
//	HAJIME_WA_WEBHOOK      URL to post inbound messages to (optional)
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"log"
	"net/http"
	"os"
	"os/signal"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/mdp/qrterminal/v3"
	"go.mau.fi/whatsmeow"
	waProto "go.mau.fi/whatsmeow/binary/proto"
	"go.mau.fi/whatsmeow/store/sqlstore"
	"go.mau.fi/whatsmeow/types"
	"go.mau.fi/whatsmeow/types/events"
	waLog "go.mau.fi/whatsmeow/util/log"
	"google.golang.org/protobuf/proto"

	_ "github.com/mattn/go-sqlite3"
)

// Session states mirror WAHA's vocabulary so dashboards and the Rust side
// need no translation table.
const (
	statusWorking  = "WORKING"
	statusScanQR   = "SCAN_QR_CODE"
	statusStarting = "STARTING"
	statusFailed   = "FAILED"
)

type bridge struct {
	client     *whatsmeow.Client
	webhookURL string
	http       *http.Client

	mu     sync.RWMutex
	status string
}

func (b *bridge) setStatus(s string) {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.status != s {
		log.Printf("session status: %s -> %s", b.status, s)
	}
	b.status = s
}

func (b *bridge) currentStatus() string {
	b.mu.RLock()
	defer b.mu.RUnlock()
	return b.status
}

// phone reports the paired number, empty when not paired.
func (b *bridge) phone() string {
	if b.client == nil || b.client.Store == nil || b.client.Store.ID == nil {
		return ""
	}
	return b.client.Store.ID.User
}

// ---------------------------------------------------------------- HTTP API

type sessionResponse struct {
	Name   string `json:"name"`
	Status string `json:"status"`
	Phone  string `json:"phone,omitempty"`
}

type sendRequest struct {
	Session string `json:"session"`
	ChatID  string `json:"chatId"`
	Text    string `json:"text"`
}

type sendResponse struct {
	ID     string `json:"id"`
	ChatID string `json:"chat_id"`
}

func writeJSON(w http.ResponseWriter, code int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(code)
	_ = json.NewEncoder(w).Encode(v)
}

func writeError(w http.ResponseWriter, code int, msg string) {
	writeJSON(w, code, map[string]string{"error": msg})
}

func (b *bridge) handleSession(w http.ResponseWriter, r *http.Request) {
	name := strings.TrimPrefix(r.URL.Path, "/session/")
	if name == "" {
		name = "default"
	}
	writeJSON(w, http.StatusOK, sessionResponse{
		Name:   name,
		Status: b.currentStatus(),
		Phone:  b.phone(),
	})
}

func (b *bridge) handleSendText(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost {
		writeError(w, http.StatusMethodNotAllowed, "POST only")
		return
	}

	var req sendRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "malformed body: "+err.Error())
		return
	}
	if strings.TrimSpace(req.ChatID) == "" || strings.TrimSpace(req.Text) == "" {
		writeError(w, http.StatusBadRequest, "chatId and text are both required")
		return
	}

	// Refuse before attempting. A caller that receives an id is entitled to
	// believe the message left this process.
	if s := b.currentStatus(); s != statusWorking {
		writeError(w, http.StatusConflict, "session is "+s+", not connected")
		return
	}

	jid, err := parseChatID(req.ChatID)
	if err != nil {
		writeError(w, http.StatusBadRequest, err.Error())
		return
	}

	ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
	defer cancel()

	resp, err := b.client.SendMessage(ctx, jid, &waProto.Message{
		Conversation: proto.String(req.Text),
	})
	if err != nil {
		log.Printf("send to %s failed: %v", req.ChatID, err)
		writeError(w, http.StatusBadGateway, "send failed: "+err.Error())
		return
	}

	writeJSON(w, http.StatusOK, sendResponse{ID: resp.ID, ChatID: req.ChatID})
}

// parseChatID accepts both WAHA's `9665...@c.us` form and a bare number.
func parseChatID(raw string) (types.JID, error) {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return types.JID{}, errors.New("chatId is empty")
	}
	// WAHA writes @c.us; whatsmeow expects @s.whatsapp.net for users.
	if strings.HasSuffix(raw, "@c.us") {
		raw = strings.TrimSuffix(raw, "@c.us") + "@" + types.DefaultUserServer
	}
	if !strings.Contains(raw, "@") {
		raw = raw + "@" + types.DefaultUserServer
	}
	jid, err := types.ParseJID(raw)
	if err != nil {
		return types.JID{}, fmt.Errorf("unusable chatId %q: %w", raw, err)
	}
	return jid, nil
}

// ------------------------------------------------------------- inbound path

// webhookEvent is WAHA's shape. The n8n workflow reads exactly these fields:
// `event`, `session`, `payload.from`, `payload.fromMe`, `payload.body`.
// Changing any name here breaks that workflow silently, so they are fixed.
type webhookEvent struct {
	Event   string         `json:"event"`
	Session string         `json:"session"`
	Payload webhookPayload `json:"payload"`
}

type webhookPayload struct {
	ID        string `json:"id"`
	From      string `json:"from"`
	FromMe    bool   `json:"fromMe"`
	Body      string `json:"body"`
	Timestamp int64  `json:"timestamp"`
}

func (b *bridge) onEvent(raw any) {
	switch evt := raw.(type) {
	case *events.Message:
		b.forwardMessage(evt)
	case *events.Connected:
		b.setStatus(statusWorking)
	case *events.Disconnected:
		b.setStatus(statusStarting)
	case *events.LoggedOut:
		b.setStatus(statusScanQR)
		log.Printf("logged out by WhatsApp: re-pair with a QR scan")
	}
}

func (b *bridge) forwardMessage(evt *events.Message) {
	if b.webhookURL == "" {
		return
	}
	text := evt.Message.GetConversation()
	if text == "" {
		if ext := evt.Message.GetExtendedTextMessage(); ext != nil {
			text = ext.GetText()
		}
	}
	if text == "" {
		return // media and receipts are not what the workflow consumes
	}

	// Report the sender in the form the workflow already handles.
	from := evt.Info.Sender.User + "@c.us"

	body, err := json.Marshal(webhookEvent{
		Event:   "message",
		Session: "default",
		Payload: webhookPayload{
			ID:        evt.Info.ID,
			From:      from,
			FromMe:    evt.Info.IsFromMe,
			Body:      text,
			Timestamp: evt.Info.Timestamp.Unix(),
		},
	})
	if err != nil {
		log.Printf("could not encode webhook: %v", err)
		return
	}

	resp, err := b.http.Post(b.webhookURL, "application/json", strings.NewReader(string(body)))
	if err != nil {
		log.Printf("webhook post failed: %v", err)
		return
	}
	defer resp.Body.Close()
	if resp.StatusCode >= 400 {
		log.Printf("webhook returned HTTP %d", resp.StatusCode)
	}
}

// ------------------------------------------------------------------- main

func env(key, fallback string) string {
	if v := strings.TrimSpace(os.Getenv(key)); v != "" {
		return v
	}
	return fallback
}

func main() {
	var (
		bind    = env("HAJIME_WA_BRIDGE_BIND", "127.0.0.1:3001")
		store   = env("HAJIME_WA_STORE", "./hajime-wa.db")
		webhook = env("HAJIME_WA_WEBHOOK", "")
	)

	logger := waLog.Stdout("bridge", "INFO", true)

	// whatsmeow takes a context on store operations. Kept at the top so a
	// hung database call during startup can be cancelled rather than
	// blocking the process forever.
	startCtx, startCancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer startCancel()

	container, err := sqlstore.New(startCtx, "sqlite3", "file:"+store+"?_foreign_keys=on", logger)
	if err != nil {
		log.Fatalf("could not open session store %s: %v", store, err)
	}

	device, err := container.GetFirstDevice(startCtx)
	if err != nil {
		log.Fatalf("could not read device from store: %v", err)
	}

	b := &bridge{
		client:     whatsmeow.NewClient(device, logger),
		webhookURL: webhook,
		http:       &http.Client{Timeout: 15 * time.Second},
		status:     statusStarting,
	}
	b.client.AddEventHandler(b.onEvent)

	if b.client.Store.ID == nil {
		// No device yet: pair by QR. Printed to the terminal because this
		// happens once, by a human, at install time.
		qrChan, _ := b.client.GetQRChannel(context.Background())
		if err := b.client.Connect(); err != nil {
			log.Fatalf("connect failed: %v", err)
		}
		b.setStatus(statusScanQR)
		go func() {
			for evt := range qrChan {
				switch evt.Event {
				case "code":
					fmt.Println("\nScan this with WhatsApp > Linked devices:")
					qrterminal.GenerateHalfBlock(evt.Code, qrterminal.L, os.Stdout)
				case "success":
					log.Printf("pairing succeeded")
				case "timeout":
					b.setStatus(statusFailed)
					log.Printf("pairing timed out; restart to try again")
				}
			}
		}()
	} else {
		if err := b.client.Connect(); err != nil {
			log.Fatalf("connect failed: %v", err)
		}
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/session/", b.handleSession)
	mux.HandleFunc("/send/text", b.handleSendText)
	mux.HandleFunc("/health", func(w http.ResponseWriter, _ *http.Request) {
		writeJSON(w, http.StatusOK, map[string]string{"status": b.currentStatus()})
	})

	server := &http.Server{
		Addr:              bind,
		Handler:           mux,
		ReadHeaderTimeout: 10 * time.Second,
	}

	go func() {
		log.Printf("bridge listening on http://%s (store=%s webhook=%q)", bind, store, webhook)
		if err := server.ListenAndServe(); err != nil && !errors.Is(err, http.ErrServerClosed) {
			log.Fatalf("http server failed: %v", err)
		}
	}()

	// Disconnect cleanly so the session survives the restart.
	stop := make(chan os.Signal, 1)
	signal.Notify(stop, os.Interrupt, syscall.SIGTERM)
	<-stop
	log.Printf("shutting down")

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	_ = server.Shutdown(ctx)
	b.client.Disconnect()
}
