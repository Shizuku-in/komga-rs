const crypto = require('node:crypto')
const http = require('node:http')

const authorizationCodes = new Map()

const server = http.createServer((request, response) => {
  const url = new URL(request.url, 'http://127.0.0.1')
  if (request.method === 'GET' && url.pathname === '/authorize') {
    const redirectUri = url.searchParams.get('redirect_uri')
    const state = url.searchParams.get('state')
    const challenge = url.searchParams.get('code_challenge')
    if (!redirectUri || !state || !challenge || url.searchParams.get('code_challenge_method') !== 'S256') {
      response.writeHead(400).end('invalid authorization request')
      return
    }
    const code = crypto.randomBytes(24).toString('base64url')
    authorizationCodes.set(code, challenge)
    const callback = new URL(redirectUri)
    callback.searchParams.set('code', code)
    callback.searchParams.set('state', state)
    response.writeHead(302, {location: callback.toString()}).end()
    return
  }

  if (request.method === 'POST' && url.pathname === '/token') {
    readRequestBody(request, response, body => {
      const parameters = new URLSearchParams(body)
      const code = parameters.get('code')
      const verifier = parameters.get('code_verifier')
      const expected = code ? authorizationCodes.get(code) : undefined
      const actual = verifier
        ? crypto.createHash('sha256').update(verifier).digest('base64url')
        : undefined
      if (!code || !expected || expected !== actual) {
        json(response, 400, {error: 'invalid_grant'})
        return
      }
      authorizationCodes.delete(code)
      json(response, 200, {access_token: 'e2e-access-token', token_type: 'Bearer'})
    })
    return
  }

  if (request.method === 'GET' && url.pathname === '/user') {
    if (request.headers.authorization !== 'Bearer e2e-access-token') {
      json(response, 401, {error: 'invalid_token'})
      return
    }
    json(response, 200, {email: 'admin@example.org'})
    return
  }

  response.writeHead(404).end()
})

server.listen(0, '127.0.0.1', () => {
  const address = server.address()
  process.stdout.write(`listening address=127.0.0.1:${address.port}\n`)
})

function readRequestBody(request, response, callback) {
  let body = ''
  request.setEncoding('utf8')
  request.on('data', chunk => {
    body += chunk
    if (body.length > 64 * 1024) request.destroy()
  })
  request.on('end', () => callback(body))
  request.on('error', () => response.destroy())
}

function json(response, status, value) {
  const body = JSON.stringify(value)
  response.writeHead(status, {
    'cache-control': 'no-store',
    'content-length': Buffer.byteLength(body),
    'content-type': 'application/json',
  })
  response.end(body)
}

for (const signal of ['SIGINT', 'SIGTERM']) {
  process.on(signal, () => server.close(() => process.exit(0)))
}
