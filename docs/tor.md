# **Tor Setup & Configuration Guide**

This guide covers:
- Installing Tor  
- Configuring Tor settings  
- Setting up a Hidden Service  
- Configuring the Control Port (with/without password)  
- Setting the SOCKS Port  

---

## **1. Installing Tor**
### **Linux (Debian/Ubuntu)**
```bash
sudo apt update
sudo apt install tor -y
```

### MacOS
```bash
brew install tor
```

---

## **2. Configuring Tor (`torrc` File)**
### **Locate & Edit `torrc`**
### Linux
```bash
sudo nano /etc/tor/torrc
```
### MacOS
```bash
nano /opt/homebrew/etc/tor/torrc
```

---

## **4. Configuring the SOCKS Proxy**
Tor acts as a **SOCKS5 Proxy** for anonymous traffic.

Add this to `torrc`:
```ini
SOCKSPort 9050
```
Now, you can route applications through `127.0.0.1:9050`.

To test it:
```bash
sudo systemctl start tor
curl --socks5-hostname 127.0.0.1:9050 https://check.torproject.org/
```
---

## **3. Configuring Control Port**
The **Control Port** allows applications to talk to Tor.
```ini
ControlPort 9051
```

### **Option 1: No Authentication (Not Recommended for Production)**
```ini
CookieAuthentication 0
```
This allows unrestricted access—use it **only for testing**.

### **Option 2: Password Authentication (Recommended)**
1. Generate a hashed password:
   ```bash
   tor --hash-password "yourpassword"
   ```
   Example output:
   ```
   16:872860B76453A77D60CA2BB8C1A7042072093276A3D701AD684053EC4C
   ```
2. Add it to `torrc`:
   ```ini
   HashedControlPassword 16:872860B76453A77D60CA2BB8C1A7042072093276A3D701AD684053EC4C
   ```

### **Option 3: Cookie Authentication**
1. Enable cookie authentication in `torrc`:
   ```ini
   ControlPort 9051
   CookieAuthentication 1
   CookieAuthFileGroupReadable 1
   DataDirectoryGroupReadable 1
   ```
2. Restart Tor:
   ```bash
   sudo systemctl restart tor
   ```
3. The **cookie file** is usually located at:
   ```bash
   /var/lib/tor/control_auth_cookie
   ```
4. Use it in your applications for authentication.




