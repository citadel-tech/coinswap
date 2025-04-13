# **Tor Setup & Configuration Guide**

This guide will help you:
- Install Tor
- Configure Tor via the `torrc` file
- Set up a Hidden Service
- Configure the ControlPort (with/without password)
- Use Tor as a SOCKS5 proxy

---

## **1. Installing Tor**

### **Linux (Debian/Ubuntu)**
```bash
sudo apt update
sudo apt install tor -y
```

Tor will run as a system service and automatically start on boot.

### **macOS**
```bash
brew install tor
```

Once installed, you can start Tor using:
```bash
brew services start tor
```

---

## **2. Configuring Tor (`torrc` File)**

The main configuration file for Tor is called `torrc`.

### **Locate and Edit the `torrc` File**

#### **Linux**
```bash
sudo nano /etc/tor/torrc
```

#### **macOS (Homebrew installation)**
```bash
nano /opt/homebrew/etc/tor/torrc
```

Make the following changes as needed.

---

## **3. Configuring the Control Port**

The ControlPort allows external applications to control Tor (e.g., changing circuits or querying status).

Add to your `torrc`:
```ini
ControlPort 9051
```

### **Option 1: No Authentication (Not Recommended for Production)**
```ini
CookieAuthentication 0
```
This disables authentication—**only use for testing in secure environments.**

---

### **Option 2: Password Authentication (Recommended for Scripts/Apps)**

1. Generate a hashed password:
   ```bash
   tor --hash-password "yourpassword"
   ```
   Example output:
   ```
   16:872860B76453A77D60CA2BB8C1A7042072093276A3D701AD684053EC4C
   ```

2. Add the hashed password to `torrc`:
   ```ini
   HashedControlPassword 16:872860B76453A77D60CA2BB8C1A7042072093276A3D701AD684053EC4C
   ```

---

### **Option 3: Cookie Authentication**

Useful when you want to use local apps like `stem` with minimal manual setup.

In `torrc`, enable:
```ini
ControlPort 9051
CookieAuthentication 1
CookieAuthFileGroupReadable 1
DataDirectoryGroupReadable 1
```

Restart Tor:
```bash
sudo systemctl restart tor
```

The control cookie is usually found at:
```bash
/var/lib/tor/control_auth_cookie
```

---

## **4. Configuring the SOCKS Proxy**

Tor runs as a **SOCKS5 proxy** on port `9050` by default.

Add to your `torrc` (if not already present):
```ini
SOCKSPort 9050
```

You can now route applications through Tor using:
- Host: `127.0.0.1`
- Port: `9050`
- Protocol: SOCKS5

To test if Tor is working correctly:
```bash
sudo systemctl start tor
curl --socks5-hostname 127.0.0.1:9050 https://check.torproject.org/
```

If you see the message "Congratulations. This browser is configured to use Tor," you're good to go.

---

## **5. Optional: Setting Up a Hidden Service**

To expose a service (e.g., a Bitcoin wallet RPC or web UI) over Tor:

1. In your `torrc`, add:
   ```ini
   HiddenServiceDir /var/lib/tor/hidden_service/
   HiddenServicePort 80 127.0.0.1:8080
   ```

   This makes port 8080 of your local machine available as port 80 on a `.onion` address.

2. Restart Tor:
   ```bash
   sudo systemctl restart tor
   ```

3. Find your `.onion` address:
   ```bash
   sudo cat /var/lib/tor/hidden_service/hostname
   ```

This address can be used to access your service anonymously over the Tor network.

---

## **6. References**

- [Tor Project Documentation](http://blog.torproject.org/meet-new-torprojectorg/)
- [Stem (Python Controller Library)](https://stem.torproject.org/)
- [Using Tor with cURL](https://curl.se/docs/manual.html)