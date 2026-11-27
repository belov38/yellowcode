pub fn intro_ticket(user: &str) -> Vec<u8> {
    let u = user.as_bytes();
    let mut out = b"V8IntroTicketReq".to_vec();
    out.extend_from_slice(&(u.len().min(usize::from(u16::MAX)) as u16).to_le_bytes());
    out.extend_from_slice(&u[..u.len().min(usize::from(u16::MAX))]);
    out.push(0);
    out
}

const OS_VERSION: [u8; 8] = [0x0a, 0x00, 0xf4, 0x65, 0x00, 0x00, 0x00, 0x0f];

pub fn negotiate() -> Vec<u8> {
    const DOMAIN: &[u8] = b"WORKGROUP";
    const HEADER: u32 = 40;
    let mut m = b"NTLMSSP\0".to_vec();
    m.extend_from_slice(&1u32.to_le_bytes());
    m.extend_from_slice(&0xE208_B2B7u32.to_le_bytes());
    let len = DOMAIN.len() as u16;
    m.extend_from_slice(&len.to_le_bytes());
    m.extend_from_slice(&len.to_le_bytes());
    m.extend_from_slice(&HEADER.to_le_bytes());
    m.extend_from_slice(&0u16.to_le_bytes());
    m.extend_from_slice(&0u16.to_le_bytes());
    m.extend_from_slice(&HEADER.to_le_bytes());
    m.extend_from_slice(&OS_VERSION);
    m.extend_from_slice(DOMAIN);
    m
}

pub fn authenticate_anonymous(mic: &[u8; 16]) -> Vec<u8> {
    const OFFSET: u32 = 88;
    let mut m = b"NTLMSSP\0".to_vec();
    m.extend_from_slice(&3u32.to_le_bytes());
    for _ in 0..6 {
        m.extend_from_slice(&0u16.to_le_bytes());
        m.extend_from_slice(&0u16.to_le_bytes());
        m.extend_from_slice(&OFFSET.to_le_bytes());
    }
    m.extend_from_slice(&0xE288_C235u32.to_le_bytes());
    m.extend_from_slice(&OS_VERSION);
    m.extend_from_slice(mic);
    m
}
