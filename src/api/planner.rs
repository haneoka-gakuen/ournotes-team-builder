//! Event-local settlement tables share immutable gameplay data and retain warm search contexts.
use super::*;

impl Engine {
    pub(crate) fn evaluate_one(&mut self,input:&Value)->Result<Value,String> {
        if self.planner_scope(input).is_some(){return self.with_planner(input,Self::evaluate_one);}
        self.evaluate(&input["request"],&input["team"],master::int(&input["song"]["songId"]),master::int(&input["song"]["difficulty"]))
    }
    pub(crate) fn qualify(&mut self,input:&Value)->Result<Value,String> {
        if self.planner_scope(input).is_some(){return self.with_planner(input,Self::qualify);}
        Ok(json!({"issues":crate::pt::validate(&self.master,&input["request"])?}))
    }
    pub(super) fn planner_scope(&self,input:&Value)->Option<(i32,bool)> {
        let request=&input["request"];
        let event=if request["goal"]["eventId"].is_number(){&request["goal"]}else{&request["event"]};
        let id=event["eventId"].as_i64().or_else(||request["challengeEventId"].as_i64())
            .or_else(||request["goal"]["challengeEventId"].as_i64())
            .or_else(||request["timing"]["challengeEventId"].as_i64())? as i32;
        let challenge=event["route"]=="challenge"||(event["route"]=="skip"&&master::int(&event["consumption"])>10)
            ||event["eventId"].is_null();
        let key=(id,challenge);
        if self.planner_key==Some(key){return None;}
        self.master.events.get(&id)?.planner_rules.is_object().then_some(key)
    }
    pub(super) fn with_planner(&mut self,input:&Value,call:impl FnOnce(&mut Engine,&Value)->Result<Value,String>)->Result<Value,String> {
        let key=self.planner_scope(input).ok_or("planner-scope")?;
        let mut scoped=if let Some(index)=self.planner_views.iter().position(|entry|entry.0==key){
            self.planner_views.remove(index).unwrap().1
        }else{
            let base=self.planner_base.as_ref().unwrap_or(&self.master);
            let mut master=base.clone();
            let rules=&base.events[&key.0].planner_rules;
            let boosts=|name:&str|master::rows(&rules[name]).iter().map(|row|master::Boost{
                consumed:master::int(&row["consumed"]),event_point_rate:master::number(&row["eventPointRate"],1.0),
                reward_rate:master::number(&row["rewardRate"],1.0)}).collect();
            if rules["boosts"].is_array(){master.boosts=Arc::new(boosts("boosts"));}
            if rules["challengeBoosts"].is_array(){master.challenge_boosts=Arc::new(boosts("challengeBoosts"));}
            if rules["liveChallengePoints"].is_array(){master.live_challenge_points=Arc::new(master::rows(&rules["liveChallengePoints"]).iter()
                .map(|row|(master::int(&row["rank"]),master::int(&row["value"]))).collect());}
            let ranks:Vec<_>=master::rows(&rules[if key.1{"challengeRanks"}else{"normalRanks"}]).iter().map(|row|master::ScoreRank{
                rank:master::int(&row["rank"]),required:master::int(&row["required"]),battle_required:master::int(&row["battleRequired"])}).collect();
            if !ranks.is_empty(){
                let group=key.0;
                Arc::make_mut(&mut master.score_ranks).insert(group,ranks);
                for song in Arc::make_mut(&mut master.songs).values_mut(){song.rank_group=group;}
            }
            let mut scoped=Box::new(Engine::from_master(master));
            scoped.planner_base=Some(base.clone());scoped.planner_key=Some(key);
            scoped.trusted_scores=self.trusted_scores;scoped.scored_seeds=self.scored_seeds.clone();
            #[cfg(not(target_arch="wasm32"))]{scoped.worker_budget=self.worker_budget;scoped.shared_floors=self.shared_floors.clone();}
            scoped
        };
        for (&(song,difficulty),source) in &self.sources{scoped.insert_chart(song,difficulty,source.clone())?;}
        let mut result=call(&mut scoped,input);
        for (&(song,difficulty),source) in &scoped.sources{self.insert_chart(song,difficulty,source.clone())?;}
        if let Ok(value)=&mut result {value["cachedCharts"]=json!(self.cached_chart_keys());}
        self.planner_views.push_front((key,scoped));self.planner_views.truncate(2);
        result
    }
}
