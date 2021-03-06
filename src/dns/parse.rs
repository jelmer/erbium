/*   Copyright 2020 Perry Lorier
 *
 *  Licensed under the Apache License, Version 2.0 (the "License");
 *  you may not use this file except in compliance with the License.
 *  You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 *  Unless required by applicable law or agreed to in writing, software
 *  distributed under the License is distributed on an "AS IS" BASIS,
 *  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 *  See the License for the specific language governing permissions and
 *  limitations under the License.
 *
 *  SPDX-License-Identifier: Apache-2.0
 *
 *  Code to parse a DNS packet.
 */
use crate::dns::dnspkt;
use std::collections::BTreeMap;

pub struct EdnsParser<'l> {
    buffer: &'l [u8],
}

impl<'l> EdnsParser<'l> {
    fn new(buffer: &'l [u8]) -> EdnsParser {
        EdnsParser { buffer }
    }

    fn get_u8(&mut self) -> Result<u8, String> {
        if let Some((first, rest)) = self.buffer.split_first() {
            self.buffer = rest;
            Ok(*first)
        } else {
            Err("Truncated EDNS Option".to_string())
        }
    }

    fn get_u16(&mut self) -> Result<u16, String> {
        let upper = self.get_u8()?;
        let lower = self.get_u8()?;
        Ok((upper as u16) * 256 + (lower as u16))
    }

    fn get_option(&mut self) -> Result<dnspkt::EdnsOption, String> {
        let code = self.get_u16()?;
        let len = self.get_u16()? as usize;
        let data = self.buffer[0..len].to_vec();
        self.buffer = &self.buffer[len..];
        if data.len() < len {
            return Err("Truncated EDNS Option".to_string());
        }
        Ok(dnspkt::EdnsOption {
            code: dnspkt::EdnsCode(code),
            data,
        })
    }

    fn get_options(&mut self) -> Result<dnspkt::EdnsData, String> {
        let mut data = dnspkt::EdnsData { other: vec![] };

        while !self.buffer.is_empty() {
            let ednsopt = self.get_option()?;
            // TODO: Understand a few obvious edns options.
            data.other.push(ednsopt);
        }

        Ok(data)
    }
}

struct Label {
    label: dnspkt::Label,
    next: Option<u16>,
}

pub struct PktParser<'l> {
    buffer: &'l [u8],
    offset: usize,
    labels: BTreeMap<u16, Label>,
}

impl<'l> PktParser<'l> {
    pub fn new(buffer: &'l [u8]) -> PktParser {
        PktParser {
            buffer,
            offset: 0,
            labels: BTreeMap::new(),
        }
    }
    fn peek_u8(&mut self) -> Result<u8, String> {
        Ok(self.buffer[self.offset])
    }
    fn get_u8(&mut self) -> Result<u8, String> {
        let ret = self.peek_u8()?;
        self.offset += 1;
        Ok(ret)
    }
    fn get_u16(&mut self) -> Result<u16, String> {
        Ok((self.get_u8()? as u16) * 256 + (self.get_u8()? as u16))
    }
    fn get_u32(&mut self) -> Result<u32, String> {
        Ok((self.get_u8()? as u32) * (256 * 256 * 256)
            + (self.get_u8()? as u32) * (256 * 256)
            + (self.get_u8()? as u32) * (256)
            + (self.get_u8()? as u32))
    }

    fn get_bytes(&mut self, count: usize) -> Result<Vec<u8>, String> {
        let ret = self.buffer[self.offset..self.offset + count].to_vec();
        self.offset += count;
        Ok(ret)
    }
    fn get_label(&mut self) -> Result<dnspkt::Label, String> {
        let size = self.get_u8()? as usize;
        assert!(size & 0b1100_0000 == 0b0000_0000);
        Ok(dnspkt::Label::from(self.get_bytes(size)?))
    }

    fn get_domain(&mut self) -> Result<dnspkt::Domain, String> {
        let mut domainv = Vec::new();
        loop {
            let prefix = self.peek_u8()?;
            match prefix & 0b1100_0000 {
                0b0000_00000 => {
                    // Uncompressed label
                    let saved_offset = self.offset as u16;
                    if self.peek_u8()? == 0 {
                        self.get_u8()?; // Consume the \0
                        return Ok(dnspkt::Domain::from(domainv));
                    }
                    let label = self.get_label()?;
                    let next = if self.peek_u8()? == 0 {
                        None
                    } else {
                        Some(self.offset as u16)
                    };
                    self.labels.insert(
                        saved_offset,
                        Label {
                            label: label.clone(),
                            next,
                        },
                    );
                    domainv.push(label.clone());
                }
                0b1100_0000 => {
                    // Compressed label.
                    let mut offset = self.get_u16()? & 0b0011_1111;
                    loop {
                        match self.labels.get(&offset) {
                            None => return Err(String::from("Bad compression offset")),
                            Some(l) => {
                                domainv.push(l.label.clone());
                                match l.next {
                                    Some(o) => {
                                        offset = o;
                                    }
                                    None => {
                                        return Ok(dnspkt::Domain::from(domainv));
                                    }
                                }
                            }
                        }
                    }
                }
                _ => return Err(String::from("Unsupported label type")),
            }
        }
    }

    fn get_class(&mut self) -> Result<dnspkt::Class, String> {
        Ok(dnspkt::Class(self.get_u16()?))
    }

    fn get_type(&mut self) -> Result<dnspkt::Type, String> {
        Ok(dnspkt::Type(self.get_u16()?))
    }

    fn get_soa(&mut self) -> Result<dnspkt::SoaData, String> {
        let _rdlen = self.get_u16()? as usize;
        Ok(dnspkt::SoaData {
            mname: self.get_domain()?,
            rname: self.get_domain()?,
            serial: self.get_u32()?,
            refresh: self.get_u32()?,
            retry: self.get_u32()?,
            expire: self.get_u32()?,
            minimum: self.get_u32()?,
        })
    }

    fn get_rdata(&mut self, rtype: dnspkt::Type) -> Result<dnspkt::RData, String> {
        match rtype {
            dnspkt::RR_OPT => {
                let rdlen = self.get_u16()? as usize;
                let rdata = self.get_bytes(rdlen)?;
                Ok(dnspkt::RData::OPT(EdnsParser::new(&rdata).get_options()?))
            }
            dnspkt::RR_SOA => Ok(dnspkt::RData::SOA(self.get_soa()?)),
            _ => {
                let rdlen = self.get_u16()? as usize;
                let rdata = self.get_bytes(rdlen)?;
                Ok(dnspkt::RData::Other(rdata))
            }
        }
    }

    fn get_rr(&mut self) -> Result<dnspkt::RR, String> {
        let domain = self.get_domain()?;
        let rrtype = self.get_type()?;
        let class = self.get_class()?;
        let ttl = self.get_u32()?;
        let rdata = self.get_rdata(rrtype)?;

        Ok(dnspkt::RR {
            domain,
            rrtype,
            class,
            ttl,
            rdata,
        })
    }

    pub fn get_dns(&mut self) -> Result<dnspkt::DNSPkt, String> {
        let qid = self.get_u16()?;
        let flag1 = self.get_u8()?;
        let flag2 = self.get_u8()?;
        let qcount = self.get_u16()?;

        let opcode = dnspkt::Opcode((flag1 & 0b0111_1000) >> 3);
        let rcode = dnspkt::RCode((flag2 & 0b0000_1111) as u16);
        if qcount != 1 {
            return Err(format!(
                "Incorrect number of questions ({} / {:?} / {:?})",
                qcount, opcode, rcode
            ));
        }
        let arcount = self.get_u16()?;
        let nscount = self.get_u16()?;
        let adcount = self.get_u16()?;

        let qdomain = self.get_domain()?;
        let qtype = self.get_type()?;
        let qclass = self.get_class()?;

        let answer = (0..arcount)
            .map(|_| self.get_rr())
            .collect::<Result<Vec<_>, _>>()?;
        let nameserver = (0..nscount)
            .map(|_| self.get_rr())
            .collect::<Result<Vec<_>, _>>()?;
        let mut additional = (0..adcount)
            .map(|_| self.get_rr())
            .collect::<Result<Vec<_>, _>>()?;

        let opt = additional.iter().find(|it| it.rrtype == dnspkt::RR_OPT);

        let bufsize = std::cmp::max(opt.map_or(512, |o| o.class.0), 512);
        let ercode = opt.map_or(0, |o| o.ttl >> 24);
        let ever = opt.map(|o| ((o.ttl >> 16) & 0xFF) as u8);
        let edo = opt.map_or(false, |o| {
            (o.ttl & 0b00000000_00000000_10000000_00000000) != 0
        });

        let edns = opt.map(|x| match &x.rdata {
            dnspkt::RData::OPT(o) => o.clone(),
            _ => panic!("opt record does not contain opt data"),
        });

        additional.retain(|rr| rr.rrtype != dnspkt::RR_OPT);

        Ok(dnspkt::DNSPkt {
            qid,
            rd: (flag1 & 0b0000_0001) != 0,
            tc: (flag1 & 0b0000_0010) != 0,
            aa: (flag1 & 0b0000_0100) != 0,
            qr: (flag1 & 0b1000_0000) != 0,
            opcode,

            cd: (flag2 & 0b0010_0000) != 0,
            ad: (flag2 & 0b0100_0000) != 0,
            ra: (flag2 & 0b1000_0000) != 0,
            //           0b0001_0000
            rcode: dnspkt::RCode(((flag2 & 0b0000_1111) as u16) | ((ercode as u16) << 8)),
            bufsize,
            edns_ver: ever,
            edns_do: edo,
            question: dnspkt::Question {
                qdomain,
                qtype,
                qclass,
            },
            answer,
            nameserver,
            additional,
            edns,
        })
    }
}
