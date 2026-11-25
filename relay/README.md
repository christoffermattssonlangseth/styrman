# Styrman push relay

People who install Styrman from TestFlight/Homebrew don't have the Apple key that
sends notifications to the Styrman app. This tiny Cloudflare Worker holds it (as a
secret) and delivers short alerts for them.

What it sees, per notification: the phone's push token and a short alert such as
"Codex · styrman / Needs your approval" (≤ 240 characters; anything else is
rejected). No commands, code or replies: Styrman always turns details off when it
uses a relay. Each Mac registers once and gets its own credentials; pushes are
rate-limited per Mac (200/hour) and registrations per IP (10/hour).

## Deploy (once)

You need a Cloudflare account (the free plan is enough) and the APNs key
(`AuthKey_NQ6KX5Y2L6.p8`).

```sh
cd relay
npm install
npx wrangler login
npx wrangler kv namespace create INSTALLS     # paste the printed id into wrangler.toml
npx wrangler secret put APNS_KEY < ~/Downloads/AuthKey_NQ6KX5Y2L6.p8
npx wrangler deploy                           # prints https://styrman-push-relay.<you>.workers.dev
```

## Check it end to end

On a Mac whose phone is paired (temporarily move your own key aside, since an own
key takes precedence):

```sh
mv ~/Library/Application\ Support/Styrman/apns.json /tmp/apns.json.bak
styr push relay https://styrman-push-relay.<you>.workers.dev
styr push test          # the phone should get "Styrman" / test notification
mv /tmp/apns.json.bak ~/Library/Application\ Support/Styrman/apns.json && styr push status
```

If `styr push test` reports `relay: 502 …`, the relay couldn't talk to Apple; the
reason is Apple's (e.g. `BadDeviceToken` for a sandbox token sent to production).

## Tests

```sh
npm test     # request validation, payload shape, JWT signing (Node's test runner)
```
